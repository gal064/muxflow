use std::{
    collections::{HashMap, VecDeque},
    ffi::CString,
    fs::{self, File},
    io::{Read, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::MetadataExt,
        io::{AsRawFd, FromRawFd},
    },
    path::Path,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU32, Ordering},
        mpsc,
    },
    time::Duration,
};

use block2::RcBlock;
use objc2::{
    AnyThread, ClassType, define_class,
    rc::Retained,
    runtime::{AnyObject, ProtocolObject},
};
use objc2_foundation::{NSDictionary, NSError, NSObject, NSObjectProtocol, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNAuthorizationStatus, UNMutableNotificationContent, UNNotification,
    UNNotificationPresentationOptions, UNNotificationRequest, UNNotificationResponse,
    UNNotificationSettings, UNNotificationSound, UNUserNotificationCenter,
    UNUserNotificationCenterDelegate,
};
use phase0_core::NotificationRoute;

use super::{TEST_BODY, TEST_TITLE};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use uuid::Uuid;

const NATIVE_CALLBACK_TIMEOUT: Duration = Duration::from_secs(5);
// The authorization callback waits for a person to respond to a modal system
// prompt. Keep ordinary framework callbacks tight, but do not discard the
// first notification merely because the person took more than five seconds.
const AUTHORIZATION_PROMPT_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_PENDING_ROUTES: usize = 512;
const ROUTE_SCHEMA: &str = "ade-notification-route-v1";
/// The identifier prefix that marks the notification the user asked for.
const TEST_IDENTIFIER_PREFIX: &str = "test-";
/// `userInfo` key carrying the frontend's foreground-presentation answer.
const FOREGROUND_KEY: &str = "adeForeground";
/// The only value that suppresses. Anything else — including a payload that
/// never set the key — presents, because invisibility is this feature's bug.
const FOREGROUND_SUPPRESS: &str = "suppress";

static APP: OnceLock<AppHandle> = OnceLock::new();
static ROUTES: OnceLock<Mutex<PendingRoutes>> = OnceLock::new();
static ROUTE_DIRECTORY: OnceLock<File> = OnceLock::new();
static NEXT_RECEIPT: AtomicU32 = AtomicU32::new(1);

#[derive(Default)]
struct PendingRoutes {
    entries: HashMap<String, NotificationRoute>,
    order: VecDeque<String>,
}

impl PendingRoutes {
    fn insert(&mut self, id: String, route: NotificationRoute) {
        while self.entries.len() >= MAX_PENDING_ROUTES {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            } else {
                break;
            }
        }
        self.order.push_back(id.clone());
        self.entries.insert(id, route);
    }

    fn take(&mut self, id: &str) -> Option<NotificationRoute> {
        let route = self.entries.remove(id);
        if route.is_some() {
            self.order.retain(|entry| entry != id);
        }
        route
    }
}

/// What the delegate shows for one notification while the app is frontmost.
///
/// Foreground suppression used to be total — `will_present` completed with no
/// options at all — so while the app was frontmost the system showed nothing,
/// for every notification, including the ones the frontend had already decided
/// were not on any pane the user can see. Focus is the frontend's fact, not
/// this process's, so its answer travels *on* the notification: this reads the
/// same `userInfo` the activation path already reads, which means there is no
/// side table to keep, bound, evict, or clean up on the error path.
///
/// The `test-` prefix short-circuits it. That notification's entire job is to
/// answer "does a banner appear at all", so it must not be able to answer "no"
/// because a payload field went missing.
fn foreground_presentation(
    identifier: &str,
    user_info: &NSDictionary,
) -> UNNotificationPresentationOptions {
    if identifier.starts_with(TEST_IDENTIFIER_PREFIX) {
        return UNNotificationPresentationOptions::Banner | UNNotificationPresentationOptions::Sound;
    }
    if notification_string(user_info, FOREGROUND_KEY).as_deref() == Some(FOREGROUND_SUPPRESS) {
        return UNNotificationPresentationOptions::empty();
    }
    // Banner without Sound: the app plays its own agent cue for the same event,
    // and two sounds for one transition is worse than none.
    UNNotificationPresentationOptions::Banner
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = AnyThread]
    struct NotificationDelegate;

    unsafe impl NSObjectProtocol for NotificationDelegate {}

    unsafe impl UNUserNotificationCenterDelegate for NotificationDelegate {
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn will_present(
            &self,
            _center: &UNUserNotificationCenter,
            notification: &UNNotification,
            completion: &block2::DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            // Only the pane the user is actually looking at represents its own
            // attention in-app. Everything else is as invisible as it would be
            // with the app in the background, so it is presented.
            let request = notification.request();
            let identifier = request.identifier().to_string();
            let options = foreground_presentation(&identifier, &request.content().userInfo());
            completion.call((options,));
        }

        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        fn did_receive(
            &self,
            _center: &UNUserNotificationCenter,
            response: &UNNotificationResponse,
            completion: &block2::DynBlock<dyn Fn()>,
        ) {
            let notification = response.notification();
            let request = notification.request();
            let id = request.identifier().to_string();
            let route = if notification_route_token(request.content().userInfo(), &id) {
                let route = routes()
                    .lock()
                    .unwrap()
                    .take(&id)
                    .or_else(|| load_persisted_route(&id));
                remove_persisted_route(&id);
                route
            } else {
                None
            };
            if let (Some(app), Some(route)) = (APP.get(), route) {
                deliver_activation(app, route);
            }
            completion.call(());
        }
    }
);

fn routes() -> &'static Mutex<PendingRoutes> {
    ROUTES.get_or_init(|| Mutex::new(PendingRoutes::default()))
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationReceipt {
    pub id: u32,
    pub actionable: bool,
}

#[derive(Clone)]
pub struct NativeNotifications {
    route_directory: Option<Arc<File>>,
}

/// One notification's parameters, named. Three of these are booleans and two
/// of them are adjacent, which is the shape that silently swaps positionally.
struct Posting<'a> {
    title: &'a str,
    body: &'a str,
    identifier: String,
    /// `Some` makes the notification actionable and requires route storage.
    route: Option<NotificationRoute>,
    present_in_foreground: bool,
    sound: bool,
}

impl NativeNotifications {
    pub fn new(app: AppHandle) -> Self {
        let route_directory = app
            .path()
            .app_data_dir()
            .ok()
            .and_then(|directory| open_route_directory(&directory).ok())
            .map(Arc::new);
        if let Some(directory) = &route_directory
            && let Ok(clone) = directory.try_clone()
        {
            let _ = ROUTE_DIRECTORY.set(clone);
        }
        let _ = APP.set(app);
        let center = UNUserNotificationCenter::currentNotificationCenter();
        let delegate: Retained<NotificationDelegate> =
            unsafe { objc2::msg_send![NotificationDelegate::class(), new] };
        center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        // UNUserNotificationCenter.delegate is weak. This delegate is exactly
        // app-lifetime state and is released by process teardown.
        let _ = Retained::into_raw(delegate);
        Self { route_directory }
    }

    pub fn notify(
        &self,
        title: &str,
        body: &str,
        route: NotificationRoute,
        request_action: bool,
        present_in_foreground: bool,
    ) -> Result<NotificationReceipt, String> {
        self.post(Posting {
            title,
            body,
            identifier: Uuid::new_v4().to_string(),
            route: request_action.then_some(route),
            present_in_foreground,
            sound: false,
        })
    }

    /// The one notification a person can ask for directly.
    ///
    /// It carries no route — an actionable notification needs route storage
    /// that may not exist, and "did a banner appear" is the whole question —
    /// and it is the call that first raises the OS permission prompt on a
    /// machine where no agent event has ever fired.
    pub fn send_test_notification(&self) -> Result<NotificationReceipt, String> {
        self.post(Posting {
            title: TEST_TITLE,
            body: TEST_BODY,
            identifier: format!("{TEST_IDENTIFIER_PREFIX}{}", Uuid::new_v4()),
            route: None,
            present_in_foreground: true,
            sound: true,
        })
    }

    /// What macOS says about this app, in the five words the UI knows.
    pub fn authorization_status(&self) -> Result<String, String> {
        let center = UNUserNotificationCenter::currentNotificationCenter();
        Ok(authorization_status_name(notification_status(&center)?).to_owned())
    }

    fn post(&self, posting: Posting<'_>) -> Result<NotificationReceipt, String> {
        let Posting {
            title,
            body,
            identifier,
            route,
            present_in_foreground,
            sound,
        } = posting;
        let center = UNUserNotificationCenter::currentNotificationCenter();
        ensure_authorized(&center)?;

        let request_action = route.is_some();
        if let Some(route) = route {
            let directory = self
                .route_directory
                .as_ref()
                .ok_or_else(|| "actionable notification route storage is unavailable".to_owned())?;
            persist_route(directory, &identifier, &route)?;
            routes().lock().unwrap().insert(identifier.clone(), route);
        }
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(title));
        content.setBody(&NSString::from_str(body));
        if sound {
            content.setSound(Some(&UNNotificationSound::defaultSound()));
        }
        let mut keys: Vec<Retained<NSString>> = Vec::new();
        let mut values: Vec<Retained<NSString>> = Vec::new();
        if request_action {
            keys.push(NSString::from_str("adeRouteSchema"));
            values.push(NSString::from_str(ROUTE_SCHEMA));
            keys.push(NSString::from_str("adeRouteToken"));
            values.push(NSString::from_str(&identifier));
        }
        if !present_in_foreground {
            keys.push(NSString::from_str(FOREGROUND_KEY));
            values.push(NSString::from_str(FOREGROUND_SUPPRESS));
        }
        if !keys.is_empty() {
            let key_refs: Vec<&NSString> = keys.iter().map(|key| &**key).collect();
            let value_refs: Vec<&NSString> = values.iter().map(|value| &**value).collect();
            let typed_user_info = NSDictionary::from_slices(&key_refs, &value_refs);
            // SAFETY: NSDictionary's generic parameters are Rust-side type
            // information only; Objective-C exposes userInfo as untyped.
            let user_info: Retained<NSDictionary> =
                unsafe { Retained::cast_unchecked(typed_user_info) };
            // SAFETY: this dictionary contains only NSString keys and values,
            // which are property-list types accepted by UserNotifications.
            unsafe { content.setUserInfo(&user_info) };
        }
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &NSString::from_str(&identifier),
            &content,
            None,
        );
        let (sender, receiver) = mpsc::sync_channel(1);
        let callback = RcBlock::new(move |error: *mut NSError| {
            let result = if error.is_null() {
                Ok(())
            } else {
                Err("macOS rejected the notification request".to_owned())
            };
            let _ = sender.send(result);
        });
        center.addNotificationRequest_withCompletionHandler(&request, Some(&callback));
        if let Err(error) = receiver
            .recv_timeout(NATIVE_CALLBACK_TIMEOUT)
            .map_err(|_| "macOS notification delivery timed out".to_owned())?
        {
            routes().lock().unwrap().take(&identifier);
            remove_persisted_route(&identifier);
            return Err(error);
        }
        Ok(NotificationReceipt {
            id: NEXT_RECEIPT.fetch_add(1, Ordering::Relaxed),
            actionable: request_action,
        })
    }
}

fn route_filename(id: &str) -> Option<CString> {
    Uuid::parse_str(id)
        .ok()
        .filter(|uuid| uuid.to_string() == id)
        .and_then(|_| CString::new(format!("{id}.json")).ok())
}

fn open_route_directory(parent: &Path) -> Result<File, String> {
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let parent = CString::new(parent.as_os_str().as_bytes())
        .map_err(|_| "notification data path contains NUL".to_owned())?;
    let parent_fd = unsafe {
        libc::open(
            parent.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if parent_fd < 0 {
        return Err("notification data directory is unavailable or unsafe".into());
    }
    let parent = unsafe { File::from_raw_fd(parent_fd) };
    let leaf = c"notification-routes";
    let created = unsafe { libc::mkdirat(parent.as_raw_fd(), leaf.as_ptr(), 0o700) };
    if created < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists {
        return Err("failed to create notification route directory".into());
    }
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            leaf.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err("notification route directory is unavailable or unsafe".into());
    }
    let directory = unsafe { File::from_raw_fd(fd) };
    let metadata = directory.metadata().map_err(|error| error.to_string())?;
    if !metadata.file_type().is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
        return Err("notification route directory is not privately owned".into());
    }
    if unsafe { libc::fchmod(directory.as_raw_fd(), 0o700) } < 0 {
        return Err("failed to secure notification route directory".into());
    }
    Ok(directory)
}

fn persist_route(directory: &File, id: &str, route: &NotificationRoute) -> Result<(), String> {
    let destination = route_filename(id).ok_or_else(|| "invalid route token".to_owned())?;
    let temporary = CString::new(format!(".{id}.{}.tmp", Uuid::new_v4()))
        .map_err(|_| "invalid temporary route token".to_owned())?;
    let bytes =
        serde_json::to_vec(route).map_err(|_| "notification route is invalid".to_owned())?;
    if bytes.len() > 16 * 1024 {
        return Err("notification route exceeds the storage limit".to_owned());
    }
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            temporary.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        return Err("failed to create notification route".into());
    }
    let mut file = unsafe { File::from_raw_fd(fd) };
    let result = (|| {
        file.write_all(&bytes)?;
        file.sync_all()?;
        if unsafe {
            libc::renameat(
                directory.as_raw_fd(),
                temporary.as_ptr(),
                directory.as_raw_fd(),
                destination.as_ptr(),
            )
        } < 0
        {
            return Err(std::io::Error::last_os_error());
        }
        directory.sync_all()?;
        Ok::<_, std::io::Error>(())
    })();
    if result.is_err() {
        unsafe { libc::unlinkat(directory.as_raw_fd(), temporary.as_ptr(), 0) };
        return Err("failed to persist notification route".to_owned());
    }
    Ok(())
}

fn notification_route_token(user_info: Retained<NSDictionary>, identifier: &str) -> bool {
    notification_string(&user_info, "adeRouteSchema").as_deref() == Some(ROUTE_SCHEMA)
        && notification_string(&user_info, "adeRouteToken").as_deref() == Some(identifier)
        && Uuid::parse_str(identifier).is_ok()
}

fn notification_string(user_info: &NSDictionary, key: &str) -> Option<String> {
    let key = NSString::from_str(key);
    let key: &AnyObject = key.as_ref();
    user_info
        .objectForKey(key)?
        .downcast::<NSString>()
        .ok()
        .map(|value| value.to_string())
}

fn load_persisted_route(id: &str) -> Option<NotificationRoute> {
    load_route_from(ROUTE_DIRECTORY.get()?, id)
}

fn load_route_from(directory: &File, id: &str) -> Option<NotificationRoute> {
    let name = route_filename(id)?;
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return None;
    }
    let mut file = unsafe { File::from_raw_fd(fd) };
    let metadata = file.metadata().ok()?;
    if !metadata.file_type().is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
        || metadata.len() > 16 * 1024
    {
        return None;
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.read_to_end(&mut bytes).ok()?;
    if bytes.len() > 16 * 1024 {
        return None;
    }
    let route = serde_json::from_slice(&bytes).ok()?;
    unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) };
    Some(route)
}

fn remove_persisted_route(id: &str) {
    if let (Some(directory), Some(name)) = (ROUTE_DIRECTORY.get(), route_filename(id)) {
        unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) };
    }
}

fn ensure_authorized(center: &UNUserNotificationCenter) -> Result<(), String> {
    let status = notification_status(center)?;
    if status == UNAuthorizationStatus::NotDetermined {
        let (sender, receiver) = mpsc::sync_channel(1);
        let callback = RcBlock::new(move |granted: objc2::runtime::Bool, error: *mut NSError| {
            let _ = sender.send((granted.as_bool(), error.is_null()));
        });
        // Sound as well as Alert. Asking for Alert alone means a granted
        // notification is silent forever, and there is no second prompt: the
        // options are fixed at the first request for the life of the install.
        center.requestAuthorizationWithOptions_completionHandler(
            UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
            &callback,
        );
        let (granted, no_error) = receiver
            .recv_timeout(AUTHORIZATION_PROMPT_TIMEOUT)
            .map_err(|_| "macOS notification permission request timed out".to_owned())?;
        if !granted || !no_error {
            return Err("macOS notification permission was denied".into());
        }
        return Ok(());
    }
    if matches!(
        status,
        UNAuthorizationStatus::Authorized
            | UNAuthorizationStatus::Provisional
            | UNAuthorizationStatus::Ephemeral
    ) {
        Ok(())
    } else {
        Err("macOS notification permission is denied; enable it in System Settings".into())
    }
}

/// The framework's status, in the vocabulary the Settings panel renders.
fn authorization_status_name(status: UNAuthorizationStatus) -> &'static str {
    match status {
        UNAuthorizationStatus::NotDetermined => "notDetermined",
        UNAuthorizationStatus::Denied => "denied",
        UNAuthorizationStatus::Authorized => "authorized",
        // Provisional and Ephemeral both mean "delivered quietly, nobody said
        // yes"; the difference does not change what the user would do next.
        UNAuthorizationStatus::Provisional | UNAuthorizationStatus::Ephemeral => "provisional",
        _ => "unsupported",
    }
}

fn notification_status(center: &UNUserNotificationCenter) -> Result<UNAuthorizationStatus, String> {
    let (sender, receiver) = mpsc::sync_channel(1);
    let callback = RcBlock::new(move |settings: std::ptr::NonNull<UNNotificationSettings>| {
        let settings = unsafe { settings.as_ref() };
        let _ = sender.send(settings.authorizationStatus());
    });
    center.getNotificationSettingsWithCompletionHandler(&callback);
    receiver
        .recv_timeout(NATIVE_CALLBACK_TIMEOUT)
        .map_err(|_| "macOS notification settings query timed out".to_owned())
}

fn deliver_activation(app: &AppHandle, route: NotificationRoute) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
    let _ = app.emit(
        "notification-activated",
        serde_json::json!({
            "hostProfile": route.host_profile,
            "serverIdentity": route.server_identity,
            "sessionId": route.session_id,
            "sessionName": route.session_name,
            "windowId": route.window_id,
            "windowName": route.window_name,
            "paneId": route.pane_id,
            "agentId": route.agent_id,
            "attentionGeneration": route.attention_generation.to_string(),
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    fn route(id: &str) -> NotificationRoute {
        NotificationRoute {
            host_profile: "local".into(),
            server_identity: "server".into(),
            session_id: "$1".into(),
            session_name: "work".into(),
            window_id: "@1".into(),
            window_name: "agent".into(),
            pane_id: "%1".into(),
            agent_id: id.into(),
            attention_generation: u64::MAX,
        }
    }

    #[test]
    fn pending_routes_are_bounded_and_consumed_exactly_once() {
        let mut pending = PendingRoutes::default();
        for index in 0..=MAX_PENDING_ROUTES {
            pending.insert(index.to_string(), route(&index.to_string()));
        }
        assert_eq!(pending.entries.len(), MAX_PENDING_ROUTES);
        assert!(pending.take("0").is_none());
        assert_eq!(pending.take("1").unwrap().agent_id, "1");
        assert!(pending.take("1").is_none());
    }

    #[test]
    fn authorization_status_reads_as_one_of_the_five_words_the_ui_knows() {
        assert_eq!(
            authorization_status_name(UNAuthorizationStatus::NotDetermined),
            "notDetermined"
        );
        assert_eq!(
            authorization_status_name(UNAuthorizationStatus::Denied),
            "denied"
        );
        assert_eq!(
            authorization_status_name(UNAuthorizationStatus::Authorized),
            "authorized"
        );
        assert_eq!(
            authorization_status_name(UNAuthorizationStatus::Provisional),
            "provisional"
        );
        assert_eq!(
            authorization_status_name(UNAuthorizationStatus::Ephemeral),
            "provisional"
        );
        // A status this build has never heard of is reported as such rather
        // than being rounded down to "denied", which would tell the user to go
        // fix a setting that is not the problem.
        assert_eq!(
            authorization_status_name(UNAuthorizationStatus(99)),
            "unsupported"
        );
    }

    fn user_info(pairs: &[(&str, &str)]) -> Retained<NSDictionary> {
        let keys: Vec<Retained<NSString>> =
            pairs.iter().map(|(key, _)| NSString::from_str(key)).collect();
        let values: Vec<Retained<NSString>> = pairs
            .iter()
            .map(|(_, value)| NSString::from_str(value))
            .collect();
        let key_refs: Vec<&NSString> = keys.iter().map(|key| &**key).collect();
        let value_refs: Vec<&NSString> = values.iter().map(|value| &**value).collect();
        unsafe { Retained::cast_unchecked(NSDictionary::from_slices(&key_refs, &value_refs)) }
    }

    #[test]
    fn foreground_presentation_shows_everything_except_what_the_frontend_suppressed() {
        let id = Uuid::new_v4().to_string();
        // The frontend said the user is looking at this pane, so the app is
        // already representing the attention itself.
        assert_eq!(
            foreground_presentation(&id, &user_info(&[(FOREGROUND_KEY, FOREGROUND_SUPPRESS)])),
            UNNotificationPresentationOptions::empty()
        );
        // Anything else is presented — including a payload that says nothing,
        // because total suppression is the defect this reverses and a missing
        // field must not quietly reinstate it.
        assert_eq!(
            foreground_presentation(&id, &user_info(&[])),
            UNNotificationPresentationOptions::Banner
        );
        assert_eq!(
            foreground_presentation(&id, &user_info(&[(FOREGROUND_KEY, "present")])),
            UNNotificationPresentationOptions::Banner
        );
    }

    #[test]
    fn the_test_notification_presents_whatever_its_payload_says() {
        // Its entire job is to answer "does a banner appear at all". It must
        // not be able to answer "no" because a field went missing.
        let id = format!("{TEST_IDENTIFIER_PREFIX}{}", Uuid::new_v4());
        let options =
            foreground_presentation(&id, &user_info(&[(FOREGROUND_KEY, FOREGROUND_SUPPRESS)]));
        assert!(options.contains(UNNotificationPresentationOptions::Banner));
        assert!(options.contains(UNNotificationPresentationOptions::Sound));
    }

    #[test]
    fn actionable_route_is_private_durable_and_consumed() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = open_route_directory(temporary.path()).unwrap();
        let id = Uuid::new_v4().to_string();
        let expected = route("durable");
        persist_route(&directory, &id, &expected).unwrap();
        let path = temporary
            .path()
            .join("notification-routes")
            .join(format!("{id}.json"));
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(load_route_from(&directory, &id), Some(expected));
        assert!(!path.exists());
        assert!(route_filename("../../escape").is_none());
    }

    #[test]
    fn route_directory_rejects_a_preseeded_symlink_without_chmodding_its_target() {
        let temporary = tempfile::tempdir().unwrap();
        let foreign = temporary.path().join("foreign");
        fs::create_dir(&foreign).unwrap();
        fs::set_permissions(&foreign, fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink(&foreign, temporary.path().join("notification-routes")).unwrap();
        assert!(open_route_directory(temporary.path()).is_err());
        assert_eq!(
            fs::metadata(foreign).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }
}

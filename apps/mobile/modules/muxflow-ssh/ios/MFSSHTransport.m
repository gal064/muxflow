#import "MFSSHTransport.h"
#include <libssh2.h>
#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <netdb.h>
#include <poll.h>
#include <sys/socket.h>
#include <time.h>
#include <unistd.h>

static double monotonicSeconds(void) {
  struct timespec time;
  clock_gettime(CLOCK_MONOTONIC, &time);
  return time.tv_sec + time.tv_nsec / 1e9;
}

@interface MFSSHWrite : NSObject
@property(nonatomic, strong) NSData *data;
@property(nonatomic) NSUInteger offset;
@property(nonatomic, copy) MFSSHWriteCompletion completion;
@end
@implementation MFSSHWrite
@end

@interface MFSSHChannel : NSObject
@property(nonatomic, copy) NSString *identifier;
@property(nonatomic, copy) NSString *command;
@property(nonatomic, copy, nullable) NSString *fingerprint;
@property(nonatomic) LIBSSH2_CHANNEL *handle;
@property(nonatomic) BOOL requestedClose;
@property(nonatomic) BOOL connected;
@property(nonatomic) BOOL closedEmitted;
@property(nonatomic) double eofAt;
@property(nonatomic) BOOL execStarted;
@property(nonatomic, strong) NSMutableArray<MFSSHWrite *> *writes;
@property(nonatomic, strong) NSMutableData *stderrPending;
@property(nonatomic, copy) NSString *closeReason;
@property(nonatomic, strong, nullable) NSNumber *exitCode;
@end
@implementation MFSSHChannel
@end

typedef NS_ENUM(NSInteger, MFSSHOperation) {
  MFSSHOpen, MFSSHExec, MFSSHRead, MFSSHWriteData, MFSSHFree, MFSSHWaitClosed
};

@interface MFSSHTransport () {
  NSCondition *_condition;
  NSMutableDictionary<NSString *, MFSSHChannel *> *_channels;
  NSString *_host;
  NSInteger _port;
  NSString *_user;
  NSString *_privatePem;
  MFSSHEvent _emit;
  BOOL _started;
  BOOL _finished;
  int _socket;
  int _wake[2];
  LIBSSH2_SESSION *_session;
  NSString *_presentedFingerprint;
  NSString *_trustIdentifier;
  NSString *_initialIdentifier;
  MFSSHChannel *_openingChannel;
  MFSSHChannel *_pendingChannel;
  MFSSHOperation _pendingOperation;
  int _pendingStream;
  MFSSHWrite *_pendingWrite;
  BOOL _pendingKeepalive;
  NSString *_failure;
  BOOL _trusted;
  double _lastReceive;
}
- (void)recordReceive;
@end

// Count actual inbound bytes, including keepalive replies processed by libssh2.
// Idle channels must not be timed out merely because they produce no terminal output.
static ssize_t receiveBytes(libssh2_socket_t socket, void *buffer, size_t length, int flags, void **abstract) {
  ssize_t count = recv(socket, buffer, length, flags);
  if (count > 0) {
    MFSSHTransport *transport = (__bridge MFSSHTransport *)*abstract;
    [transport recordReceive];
  }
  return count < 0 ? -errno : count;
}

@implementation MFSSHTransport
- (instancetype)initWithHost:(NSString *)host port:(NSInteger)port user:(NSString *)user privatePem:(NSString *)privatePem event:(MFSSHEvent)event {
  if ((self = [super init])) {
    static dispatch_once_t initialized;
    dispatch_once(&initialized, ^{ libssh2_init(0); });
    _condition = [NSCondition new];
    _channels = [NSMutableDictionary new];
    _host = [host copy]; _port = port; _user = [user copy];
    _privatePem = [privatePem copy]; _emit = [event copy];
    _socket = -1; _wake[0] = -1; _wake[1] = -1;
    if (pipe(_wake) != 0) return nil;
    fcntl(_wake[0], F_SETFL, O_NONBLOCK);
    fcntl(_wake[1], F_SETFL, O_NONBLOCK);
  }
  return self;
}

- (void)dealloc {
  if (_wake[0] >= 0) close(_wake[0]);
  if (_wake[1] >= 0) close(_wake[1]);
}

- (void)recordReceive { _lastReceive = monotonicSeconds(); }

- (BOOL)acceptingChannels {
  [_condition lock];
  BOOL accepting = !_finished && (!_started || [self hasLiveChannelsLocked]);
  [_condition unlock];
  return accepting;
}

- (BOOL)hasLiveChannelsLocked {
  for (MFSSHChannel *channel in _channels.allValues) if (!channel.requestedClose) return YES;
  return NO;
}

- (void)wakeWorker {
  char byte = 0;
  (void)write(_wake[1], &byte, 1);
}

- (void)open:(NSString *)identifier command:(NSString *)command fingerprint:(NSString *)fingerprint {
  MFSSHChannel *channel = [MFSSHChannel new];
  channel.identifier = identifier; channel.command = command; channel.fingerprint = fingerprint;
  channel.writes = [NSMutableArray new];
  channel.stderrPending = [NSMutableData new];
  [_condition lock];
  if (_finished || _channels[identifier]) {
    [_condition unlock];
    _emit(@{@"type": @"closed", @"connectionId": identifier, @"reason": @"connectFailed", @"exitCode": NSNull.null});
    return;
  }
  _channels[identifier] = channel;
  BOOL start = !_started;
  if (start) _initialIdentifier = identifier;
  _started = YES;
  [_condition unlock];
  [self wakeWorker];
  if (start) dispatch_async(dispatch_get_global_queue(QOS_CLASS_USER_INITIATED, 0), ^{
    @autoreleasepool { [self run]; }
  });
}

- (void)trust:(NSString *)identifier fingerprint:(NSString *)fingerprint {
  [_condition lock];
  if ([identifier isEqualToString:_trustIdentifier] && [fingerprint isEqualToString:_presentedFingerprint]) {
    _trusted = YES;
    [_condition broadcast];
  }
  [_condition unlock];
}

- (void)write:(NSString *)identifier data:(NSData *)data completion:(MFSSHWriteCompletion)completion {
  MFSSHWrite *request = [MFSSHWrite new]; request.data = data; request.completion = completion;
  [_condition lock];
  MFSSHChannel *channel = _channels[identifier];
  if (!channel || !channel.connected || channel.requestedClose) {
    [_condition unlock]; completion(@"SSH channel is closed"); return;
  }
  [channel.writes addObject:request];
  [_condition unlock];
  [self wakeWorker];
}

- (void)close:(NSString *)identifier {
  [_condition lock];
  _channels[identifier].requestedClose = YES;
  if (![self hasLiveChannelsLocked] && _socket >= 0) shutdown(_socket, SHUT_RDWR);
  [_condition broadcast];
  [_condition unlock];
  [self wakeWorker];
}

- (BOOL)cancelled {
  [_condition lock]; BOOL cancelled = ![self hasLiveChannelsLocked]; [_condition unlock];
  return cancelled;
}

- (NSArray<MFSSHChannel *> *)channels {
  [_condition lock]; NSArray *channels = _channels.allValues; [_condition unlock]; return channels;
}

- (BOOL)connectSocket {
  struct addrinfo hints = {0}, *addresses = NULL;
  hints.ai_socktype = SOCK_STREAM;
  if (getaddrinfo(_host.UTF8String, [NSString stringWithFormat:@"%ld", (long)_port].UTF8String, &hints, &addresses) != 0) return NO;
  double deadline = monotonicSeconds() + 10;
  BOOL connected = NO;
  for (struct addrinfo *address = addresses; address && ![self cancelled]; address = address->ai_next) {
    int fd = socket(address->ai_family, address->ai_socktype, address->ai_protocol);
    if (fd < 0) continue;
    fcntl(fd, F_SETFL, O_NONBLOCK);
    int noSignal = 1;
    setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &noSignal, sizeof(noSignal));
    [_condition lock]; _socket = fd; [_condition unlock];
    int result = connect(fd, address->ai_addr, address->ai_addrlen);
    if (result < 0 && errno == EINPROGRESS) {
      while (![self cancelled] && monotonicSeconds() < deadline) {
        struct pollfd pending = {fd, POLLOUT, 0};
        int ready = poll(&pending, 1, 100);
        if (ready > 0) {
          int error = 0; socklen_t length = sizeof(error);
          result = getsockopt(fd, SOL_SOCKET, SO_ERROR, &error, &length) == 0 && error == 0 ? 0 : -1;
          break;
        }
        if (ready < 0 && errno != EINTR) break;
      }
    }
    if (result == 0 && ![self cancelled]) { connected = YES; break; }
    [_condition lock]; _socket = -1; close(fd); [_condition unlock];
  }
  freeaddrinfo(addresses);
  return connected;
}

- (NSString *)authenticate:(MFSSHChannel *)first {
  if (![self connectSocket]) return @"connectFailed";
  _session = libssh2_session_init_ex(NULL, NULL, NULL, (__bridge void *)self);
  if (!_session) return @"connectFailed";
  libssh2_session_callback_set2(_session, LIBSSH2_CALLBACK_RECV, (libssh2_cb_generic *)receiveBytes);
  libssh2_session_set_timeout(_session, 10000);
  if (libssh2_session_handshake(_session, _socket) != 0) return @"connectFailed";
  const char *hash = libssh2_hostkey_hash(_session, LIBSSH2_HOSTKEY_HASH_SHA256);
  if (!hash) return @"connectFailed";
  NSString *base64 = [[NSData dataWithBytes:hash length:32] base64EncodedStringWithOptions:0];
  _presentedFingerprint = [@"SHA256:" stringByAppendingString:[base64 stringByReplacingOccurrencesOfString:@"=" withString:@""]];
  if (first.fingerprint) {
    if (![first.fingerprint isEqualToString:_presentedFingerprint]) return @"hostKeyMismatch";
  } else {
    size_t length = 0; int type = 0;
    const char *key = libssh2_session_hostkey(_session, &length, &type);
    if (!key || length < 4) return @"connectFailed";
    uint32_t algorithmLength; memcpy(&algorithmLength, key, 4); algorithmLength = ntohl(algorithmLength);
    if (algorithmLength > length - 4) return @"connectFailed";
    NSString *algorithm = [[NSString alloc] initWithBytes:key + 4 length:algorithmLength encoding:NSUTF8StringEncoding];
    if (!algorithm) return @"connectFailed";
    [_condition lock]; _trustIdentifier = first.identifier; [_condition unlock];
    _emit(@{@"type": @"hostKey", @"connectionId": first.identifier, @"algorithm": algorithm, @"fingerprintSha256": _presentedFingerprint});
    NSDate *deadline = [NSDate dateWithTimeIntervalSinceNow:60];
    [_condition lock];
    while (!_trusted && !first.requestedClose && [deadline timeIntervalSinceNow] > 0) [_condition waitUntilDate:deadline];
    BOOL trusted = _trusted && !first.requestedClose; _trustIdentifier = nil;
    [_condition unlock];
    if (!trusted) return @"hostKeyNotTrusted";
  }
  if (muxflow_libssh2_session_pin_hostkey(_session) != 0) return @"connectFailed";
  // Tailscale check-mode auth waits for approval outside the app. Closing the
  // socket still cancels this wait; do not reuse the handshake timeout here.
  libssh2_session_set_timeout(_session, 0);
  const char *username = _user.UTF8String;
  // userauth_list sends SSH_USERAUTH_NONE; NULL also means successful none auth.
  char *methods = libssh2_userauth_list(_session, username, (unsigned int)strlen(username));
  if (muxflow_libssh2_session_hostkey_mismatch(_session)) return @"hostKeyMismatch";
  if (!libssh2_userauth_authenticated(_session)) {
    if (!methods) return @"connectFailed";
    if (!strstr(methods, "publickey") || !_privatePem) return @"authFailed";
    const char *pem = _privatePem.UTF8String;
    int result = libssh2_userauth_publickey_frommemory(_session, username, strlen(username), NULL, 0, pem, strlen(pem), NULL);
    if (muxflow_libssh2_session_hostkey_mismatch(_session)) return @"hostKeyMismatch";
    if (result != 0) return (result == LIBSSH2_ERROR_AUTHENTICATION_FAILED || result == LIBSSH2_ERROR_PUBLICKEY_UNVERIFIED || result == LIBSSH2_ERROR_KEYFILE_AUTH_FAILED) ? @"authFailed" : @"connectFailed";
  }
  _privatePem = nil;
  // Bound channel open/exec reply waits, including a cancelled pending open.
  // Apply after auth so external Tailscale approval is not subject to this cap.
  libssh2_session_set_read_timeout(_session, 10);
  libssh2_session_set_blocking(_session, 0);
  libssh2_keepalive_config(_session, 1, 15);
  _lastReceive = monotonicSeconds();
  return nil;
}

- (void)notifyClosed:(MFSSHChannel *)channel reason:(NSString *)reason exitCode:(NSNumber *)exitCode {
  [self emitStderr:channel final:YES];
  [_condition lock];
  if (channel.closedEmitted) { [_condition unlock]; return; }
  channel.closedEmitted = YES; channel.connected = NO;
  NSArray<MFSSHWrite *> *writes = [channel.writes copy]; [channel.writes removeAllObjects];
  BOOL requested = channel.requestedClose;
  [_condition unlock];
  for (MFSSHWrite *request in writes) {
    if (request.completion) request.completion(@"SSH channel is closed");
    request.completion = nil;
  }
  _emit(@{@"type": @"closed", @"connectionId": channel.identifier,
          @"reason": requested ? @"closedByClient" : reason, @"exitCode": exitCode ?: NSNull.null});
}

- (void)finish:(MFSSHChannel *)channel reason:(NSString *)reason exitCode:(NSNumber *)exitCode {
  [self notifyClosed:channel reason:reason exitCode:exitCode];
  [_condition lock]; [_channels removeObjectForKey:channel.identifier]; [_condition unlock];
}

- (void)emitStderr:(MFSSHChannel *)channel final:(BOOL)final {
  NSUInteger count = channel.stderrPending.length;
  if (!count) return;
  NSString *text = nil;
  NSUInteger consumed = count;
  // A UTF-8 code point can straddle native reads. Preserve up to three tail bytes.
  for (NSUInteger tail = 0; tail <= MIN((NSUInteger)3, count); ++tail) {
    text = [[NSString alloc] initWithBytes:channel.stderrPending.bytes length:count - tail encoding:NSUTF8StringEncoding];
    if (text) { consumed = count - tail; break; }
  }
  if (!text) {
    text = [[NSString alloc] initWithData:channel.stderrPending encoding:NSISOLatin1StringEncoding];
    consumed = count;
  }
  if (text.length) _emit(@{@"type": @"stderr", @"connectionId": channel.identifier, @"text": text});
  if (consumed) [channel.stderrPending replaceBytesInRange:NSMakeRange(0, consumed) withBytes:NULL length:0];
  if (final && channel.stderrPending.length) {
    _emit(@{@"type": @"stderr", @"connectionId": channel.identifier, @"text": @"\uFFFD"});
    [channel.stderrPending setLength:0];
  }
}

// libssh2 pins partially sent packets to the operation's original arguments.
// Retry an outbound EAGAIN before any other operation, including cancellation.
// Inbound-only waits may coexist: a blocked bulk window must not block control.
- (BOOL)perform:(MFSSHOperation)operation channel:(MFSSHChannel *)channel stream:(int)stream {
  ssize_t result = 0;
  char bytes[65536];
  MFSSHWrite *request = nil;
  switch (operation) {
    case MFSSHOpen:
      if (_openingChannel && _openingChannel != channel) return NO;
      _openingChannel = channel;
      channel.handle = libssh2_channel_open_session(_session);
      result = channel.handle ? 0 : libssh2_session_last_errno(_session);
      if (result != LIBSSH2_ERROR_EAGAIN) _openingChannel = nil;
      break;
    case MFSSHExec:
      result = libssh2_channel_exec(channel.handle, channel.command.UTF8String);
      break;
    case MFSSHRead:
      result = libssh2_channel_read_ex(channel.handle, stream, bytes, sizeof(bytes));
      break;
    case MFSSHWriteData:
      [_condition lock]; request = _pendingWrite ?: channel.writes.firstObject; [_condition unlock];
      if (!request) return NO;
      result = libssh2_channel_write(channel.handle, (const char *)request.data.bytes + request.offset, request.data.length - request.offset);
      break;
    case MFSSHWaitClosed:
      result = libssh2_channel_wait_closed(channel.handle);
      break;
    case MFSSHFree:
      result = muxflow_libssh2_channel_free_no_wait(channel.handle);
      break;
  }
  if (muxflow_libssh2_session_hostkey_mismatch(_session)) { _failure = @"hostKeyMismatch"; return NO; }
  if (result == LIBSSH2_ERROR_EAGAIN) {
    if (libssh2_session_block_directions(_session) & LIBSSH2_SESSION_BLOCK_OUTBOUND) {
      _pendingChannel = channel; _pendingOperation = operation; _pendingStream = stream;
      if (operation == MFSSHWriteData) _pendingWrite = request;
    } else {
      _pendingChannel = nil; _pendingWrite = nil;
    }
    return NO;
  }
  _pendingChannel = nil; _pendingWrite = nil;
  switch (operation) {
    case MFSSHOpen:
      if (!channel.handle) [self finish:channel reason:@"connectFailed" exitCode:nil];
      return channel.handle != NULL;
    case MFSSHExec:
      if (result != 0) channel.closeReason = @"connectFailed";
      else {
        channel.execStarted = YES;
        [_condition lock]; BOOL requested = channel.requestedClose; channel.connected = !requested; [_condition unlock];
        if (!requested) _emit(@{@"type": @"connected", @"connectionId": channel.identifier});
      }
      return YES;
    case MFSSHRead:
      if (result < 0) channel.closeReason = @"networkLost";
      if (result <= 0) return NO;
      if (!channel.closedEmitted) {
        NSData *data = [NSData dataWithBytes:bytes length:(NSUInteger)result];
        if (stream == 0) _emit(@{@"type": @"data", @"connectionId": channel.identifier, @"base64": [data base64EncodedStringWithOptions:0]});
        else { [channel.stderrPending appendData:data]; [self emitStderr:channel final:NO]; }
      }
      return YES;
    case MFSSHWriteData:
      if (result > 0) request.offset += (NSUInteger)result;
      if (request.offset == request.data.length || result < 0) {
        [_condition lock]; [channel.writes removeObject:request]; [_condition unlock];
        if (request.completion) request.completion(result < 0 ? @"SSH write failed" : nil);
        request.completion = nil;
        if (result < 0) channel.closeReason = @"networkLost";
      }
      return result > 0 || result < 0;
    case MFSSHWaitClosed:
      channel.exitCode = muxflow_libssh2_channel_has_exit_status(channel.handle) ? @(libssh2_channel_get_exit_status(channel.handle)) : nil;
      channel.closeReason = result == 0 ? @"exited" : @"networkLost";
      return YES;
    case MFSSHFree:
      channel.handle = NULL;
      [self finish:channel reason:channel.closeReason ?: @"closedByClient" exitCode:channel.exitCode];
      return YES;
  }
  return NO;
}

- (BOOL)pump:(MFSSHChannel *)channel {
  [_condition lock]; BOOL requested = channel.requestedClose; [_condition unlock];
  if (requested || channel.closeReason) {
    if (channel.handle) return [self perform:MFSSHFree channel:channel stream:0];
    // A channel-open request waiting for its reply owns session-wide open state.
    if (_openingChannel == channel) return [self perform:MFSSHOpen channel:channel stream:0];
    [self finish:channel reason:channel.closeReason ?: @"closedByClient" exitCode:channel.exitCode];
    return YES;
  }
  if (channel.fingerprint && ![channel.fingerprint isEqualToString:_presentedFingerprint]) {
    [self finish:channel reason:@"hostKeyMismatch" exitCode:nil]; return YES;
  }
  if (!channel.handle) {
    BOOL progress = [self perform:MFSSHOpen channel:channel stream:0];
    if (!channel.handle || _pendingChannel) return progress;
  }
  if (!channel.execStarted) {
    BOOL progress = [self perform:MFSSHExec channel:channel stream:0];
    if (!channel.execStarted || _pendingChannel) return progress;
  }
  BOOL more = NO;
  for (int stream = 0; stream <= 1; ++stream) {
    for (int chunk = 0; chunk < 16; ++chunk) {
      BOOL progress = [self perform:MFSSHRead channel:channel stream:stream];
      if (_pendingChannel || _failure || channel.closeReason) return progress;
      if (!progress) break;
      if (chunk == 15) more = YES;
    }
  }
  if (libssh2_channel_eof(channel.handle)) {
    // Match Android's bounded wait: EOF can arrive without status/close.
    if (!channel.eofAt) channel.eofAt = monotonicSeconds();
    if (monotonicSeconds() - channel.eofAt >= 5) {
      channel.exitCode = muxflow_libssh2_channel_has_exit_status(channel.handle) ? @(libssh2_channel_get_exit_status(channel.handle)) : nil;
      channel.closeReason = @"exited";
      return YES;
    }
    more |= [self perform:MFSSHWaitClosed channel:channel stream:0];
    return more;
  }
  return [self perform:MFSSHWriteData channel:channel stream:0] || more;
}

- (void)run {
  [_condition lock]; MFSSHChannel *first = _channels[_initialIdentifier]; [_condition unlock];
  NSString *failure = first ? [self authenticate:first] : @"closedByClient";
  double lastKeepalive = 0;
  while (!failure && self.channels.count) {
    @autoreleasepool {
      BOOL more = NO;
      for (MFSSHChannel *channel in self.channels) {
        [_condition lock]; BOOL requested = channel.requestedClose; [_condition unlock];
        if (requested) [self notifyClosed:channel reason:@"closedByClient" exitCode:nil];
      }
      if (_pendingChannel) more |= [self perform:_pendingOperation channel:_pendingChannel stream:_pendingStream];
      if (!_pendingChannel && !_pendingKeepalive) {
        for (MFSSHChannel *channel in self.channels) {
          more |= [self pump:channel];
          if (_pendingChannel || _failure) break;
        }
      }
      if (_failure) { failure = _failure; break; }
      if ([self cancelled]) break;
      double now = monotonicSeconds();
      if (now - _lastReceive >= 45) { failure = @"networkLost"; break; }
      if (!_pendingChannel && (_pendingKeepalive || now - lastKeepalive >= 15)) {
        int next = 0; int result = libssh2_keepalive_send(_session, &next);
        if (result != 0 && result != LIBSSH2_ERROR_EAGAIN) { failure = @"networkLost"; break; }
        _pendingKeepalive = result == LIBSSH2_ERROR_EAGAIN;
        if (result == 0) lastKeepalive = now;
      }
      int directions = libssh2_session_block_directions(_session);
      struct pollfd descriptors[2] = {
        {_socket, POLLIN | ((directions & LIBSSH2_SESSION_BLOCK_OUTBOUND) ? POLLOUT : 0), 0},
        {_wake[0], POLLIN, 0}
      };
      int result = poll(descriptors, 2, more ? 0 : 1000);
      if (result < 0 && errno != EINTR) { failure = @"networkLost"; break; }
      if ((descriptors[0].revents & (POLLERR | POLLHUP | POLLNVAL)) && !(descriptors[0].revents & POLLIN)) { failure = @"networkLost"; break; }
      if (descriptors[1].revents & POLLIN) {
        char byte;
        while (read(_wake[0], &byte, 1) > 0) {}
      }
    }
  }
  [_condition lock]; _finished = YES; [_condition unlock];
  for (MFSSHChannel *channel in self.channels) [self finish:channel reason:failure ?: @"closedByClient" exitCode:nil];
  [_condition lock]; int fd = _socket; _socket = -1; [_condition unlock];
  if (fd >= 0) shutdown(fd, SHUT_RDWR);
  if (_session) {
    libssh2_session_set_blocking(_session, 1);
    libssh2_session_set_timeout(_session, 1000);
    libssh2_session_free(_session); _session = NULL;
  }
  if (fd >= 0) close(fd);
  _privatePem = nil;
}

@end

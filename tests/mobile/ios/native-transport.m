// Exercises the actual iOS engine on macOS, using the same Foundation/libssh2 APIs.
// Simulator UI QA separately checks Expo bridging and the full app surface.
#import <Foundation/Foundation.h>
#import "MFSSHTransport.h"

static NSCondition *condition;
static NSMutableDictionary<NSString *, NSMutableArray<NSDictionary *> *> *events;
static NSMutableDictionary<NSString *, NSMutableData *> *stdoutBytes;

static void require(BOOL ok, NSString *message) {
  if (!ok) { fprintf(stderr, "%s\n", message.UTF8String); exit(1); }
}

static NSDictionary *waitEvent(NSString *identifier, NSString *type, NSTimeInterval timeout) {
  NSDate *deadline = [NSDate dateWithTimeIntervalSinceNow:timeout];
  [condition lock];
  while (YES) {
    for (NSDictionary *event in events[identifier]) {
      if ([event[@"type"] isEqual:type]) { [condition unlock]; return event; }
    }
    if (![condition waitUntilDate:deadline]) { [condition unlock]; require(NO, [NSString stringWithFormat:@"%@ %@ timed out", identifier, type]); }
  }
}

static MFSSHTransport *transport(NSDictionary *fixture, NSString *mode, NSString *pem) {
  return [[MFSSHTransport alloc] initWithHost:@"127.0.0.1" port:[fixture[mode] integerValue] user:@"fixture" privatePem:pem event:^(NSDictionary *event) {
    [condition lock];
    NSString *identifier = event[@"connectionId"];
    if (!events[identifier]) events[identifier] = [NSMutableArray new];
    [events[identifier] addObject:event];
    if ([event[@"type"] isEqual:@"data"]) {
      if (!stdoutBytes[identifier]) stdoutBytes[identifier] = [NSMutableData new];
      [stdoutBytes[identifier] appendData:[[NSData alloc] initWithBase64EncodedString:event[@"base64"] options:0]];
    }
    [condition broadcast]; [condition unlock];
  }];
}

static void assertClose(NSString *identifier, NSString *reason, id status) {
  NSDictionary *event = waitEvent(identifier, @"closed", 15);
  require([event[@"reason"] isEqual:reason], [NSString stringWithFormat:@"%@ wrong close reason: %@", identifier, event[@"reason"]]);
  require([event[@"exitCode"] isEqual:status], [NSString stringWithFormat:@"%@ wrong exit code: %@", identifier, event[@"exitCode"]]);
}

int main(int argc, const char **argv) {
  @autoreleasepool {
    setvbuf(stdout, NULL, _IOLBF, 0);
    require(argc == 2, @"usage: native-transport fixture.json");
    NSDictionary *fixture = [NSJSONSerialization JSONObjectWithData:[NSData dataWithContentsOfFile:@(argv[1])] options:0 error:NULL];
    require(fixture != nil, @"fixture missing");
    NSString *pem = [NSString stringWithContentsOfFile:fixture[@"key"] encoding:NSUTF8StringEncoding error:NULL];
    condition = [NSCondition new]; events = [NSMutableDictionary new]; stdoutBytes = [NSMutableDictionary new];
    NSString *fingerprint = nil;
    for (NSString *mode in @[@"none", @"publickey"]) {
      NSString *first = [mode stringByAppendingString:@"-first"];
      NSString *second = [mode stringByAppendingString:@"-second"];
      MFSSHTransport *ssh = transport(fixture, mode, [mode isEqual:@"none"] ? nil : pem);
      [ssh open:first command:@"printf 'one\\n'; printf 'diagnostic\\n' >&2; exit 17" fingerprint:nil];
      NSDictionary *hostkey = waitEvent(first, @"hostKey", 15);
      fingerprint = hostkey[@"fingerprintSha256"];
      require([hostkey[@"algorithm"] isEqual:@"ssh-rsa"], @"host-key algorithm wrong");
      [ssh trust:first fingerprint:fingerprint];
      [ssh open:second command:@"cat" fingerprint:fingerprint];
      waitEvent(first, @"connected", 15); waitEvent(second, @"connected", 15);
      assertClose(first, @"exited", @17);
      [condition lock]; NSArray *firstEvents = [events[first] copy]; NSData *firstData = [stdoutBytes[first] copy]; [condition unlock];
      require([firstData isEqual:[@"one\n" dataUsingEncoding:NSUTF8StringEncoding]], @"stdout changed");
      BOOL stderrOK = NO;
      for (NSDictionary *event in firstEvents) if ([event[@"type"] isEqual:@"stderr"] && [event[@"text"] isEqual:@"diagnostic\n"]) stderrOK = YES;
      require(stderrOK, @"stderr missing");
      // Multiple facade-sized writes retain ordering and binary data.
      NSMutableData *expected = [NSMutableData new];
      for (int chunk = 0; chunk < 8; ++chunk) {
        NSMutableData *data = [NSMutableData dataWithLength:49152];
        memset(data.mutableBytes, chunk, data.length); [expected appendData:data];
        [ssh write:second data:data completion:^(NSString *error) { require(error == nil, @"write failed"); }];
      }
      NSDate *deadline = [NSDate dateWithTimeIntervalSinceNow:10];
      [condition lock];
      while (stdoutBytes[second].length < expected.length && [condition waitUntilDate:deadline]) {}
      NSData *actual = [stdoutBytes[second] copy]; [condition unlock];
      require([actual isEqual:expected], @"chunked binary writes changed or stalled");
      // Closing one channel must leave the sibling transport usable.
      NSString *third = [mode stringByAppendingString:@"-third"];
      [ssh open:third command:@"cat" fingerprint:fingerprint]; waitEvent(third, @"connected", 15);
      [ssh close:second]; assertClose(second, @"closedByClient", NSNull.null);
      [ssh write:third data:[@"still connected\n" dataUsingEncoding:NSUTF8StringEncoding] completion:^(NSString *error) { require(error == nil, @"sibling write failed"); }];
      waitEvent(third, @"data", 15); [ssh close:third]; assertClose(third, @"closedByClient", NSNull.null);
    }
    puts("PASS native: none/publickey auth, trust, binary writes, stderr and independent channels");
    MFSSHTransport *mismatch = transport(fixture, @"none", nil);
    [mismatch open:@"mismatch" command:@"cat" fingerprint:@"SHA256:wrong"];
    assertClose(@"mismatch", @"hostKeyMismatch", NSNull.null);
    MFSSHTransport *noKey = transport(fixture, @"publickey", nil);
    [noKey open:@"no-key" command:@"cat" fingerprint:fingerprint];
    assertClose(@"no-key", @"authFailed", NSNull.null);
    MFSSHTransport *cancel = transport(fixture, @"none", nil);
    [cancel open:@"cancel" command:@"cat" fingerprint:nil];
    waitEvent(@"cancel", @"hostKey", 15); [cancel close:@"cancel"];
    assertClose(@"cancel", @"closedByClient", NSNull.null);
    MFSSHTransport *noStatus = transport(fixture, @"none", nil);
    [noStatus open:@"no-status" command:@"no-status" fingerprint:fingerprint];
    assertClose(@"no-status", @"exited", NSNull.null);
    MFSSHTransport *eofOnly = transport(fixture, @"none", nil);
    [eofOnly open:@"eof-only" command:@"eof-only" fingerprint:fingerprint];
    assertClose(@"eof-only", @"exited", NSNull.null);
    MFSSHTransport *zero = transport(fixture, @"none", nil);
    [zero open:@"zero" command:@"exit 0" fingerprint:fingerprint];
    assertClose(@"zero", @"exited", @0);
    puts("PASS native: refusal/cancel, nullable exit status and bounded EOF");
    MFSSHTransport *sameKey = transport(fixture, @"none", nil);
    [sameKey open:@"same-key" command:@"rekey-same" fingerprint:fingerprint]; waitEvent(@"same-key", @"connected", 15);
    [NSThread sleepForTimeInterval:1];
    [sameKey write:@"same-key" data:[@"after rekey" dataUsingEncoding:NSUTF8StringEncoding] completion:^(NSString *error) { require(error == nil, @"same-key rekey failed"); }];
    waitEvent(@"same-key", @"data", 15); [sameKey close:@"same-key"]; assertClose(@"same-key", @"closedByClient", NSNull.null);
    MFSSHTransport *changedKey = transport(fixture, @"none", nil);
    [changedKey open:@"changed-key" command:@"rekey-changed" fingerprint:fingerprint];
    assertClose(@"changed-key", @"hostKeyMismatch", NSNull.null);
    puts("PASS native: unchanged/changed host key during rekey");
    MFSSHTransport *openBound = transport(fixture, @"delayed-open", nil);
    [openBound open:@"open-control" command:@"cat" fingerprint:fingerprint]; waitEvent(@"open-control", @"connected", 15);
    [openBound open:@"open-cancel" command:@"cat" fingerprint:fingerprint];
    [NSThread sleepForTimeInterval:0.5]; [openBound close:@"open-cancel"];
    assertClose(@"open-cancel", @"closedByClient", NSNull.null);
    [openBound open:@"open-replacement" command:@"cat" fingerprint:fingerprint];
    waitEvent(@"open-replacement", @"connected", 15);
    [openBound write:@"open-control" data:[@"control survives" dataUsingEncoding:NSUTF8StringEncoding] completion:^(NSString *error) { require(error == nil, @"control lost after bulk cancellation"); }];
    waitEvent(@"open-control", @"data", 15);
    [openBound close:@"open-replacement"]; [openBound close:@"open-control"];
    MFSSHTransport *refused = transport(fixture, @"refused", nil);
    [refused open:@"refused" command:@"cat" fingerprint:fingerprint];
    assertClose(@"refused", @"connectFailed", NSNull.null);
    MFSSHTransport *delayed = transport(fixture, @"delayed-none", nil);
    [delayed open:@"delayed" command:@"cat" fingerprint:fingerprint];
    waitEvent(@"delayed", @"connected", 20); [delayed close:@"delayed"];
    assertClose(@"delayed", @"closedByClient", NSNull.null);
    MFSSHTransport *cancelAuth = transport(fixture, @"delayed-none", nil);
    [cancelAuth open:@"cancel-auth" command:@"cat" fingerprint:fingerprint];
    [NSThread sleepForTimeInterval:1]; [cancelAuth close:@"cancel-auth"];
    assertClose(@"cancel-auth", @"closedByClient", NSNull.null);
    puts("PASS native: cancelled channel-open, delayed auth and auth cancellation");
    MFSSHTransport *idle = transport(fixture, @"none", nil);
    [idle open:@"idle" command:@"cat" fingerprint:fingerprint]; waitEvent(@"idle", @"connected", 15);
    [NSThread sleepForTimeInterval:50];
    [idle write:@"idle" data:[@"alive" dataUsingEncoding:NSUTF8StringEncoding] completion:^(NSString *error) { require(error == nil, @"healthy idle connection timed out"); }];
    waitEvent(@"idle", @"data", 15); [idle close:@"idle"]; assertClose(@"idle", @"closedByClient", NSNull.null);
    puts("PASS native: healthy idle survives keepalive interval");
    MFSSHTransport *silent = transport(fixture, @"silent", nil);
    [silent open:@"silent" command:@"cat" fingerprint:fingerprint]; waitEvent(@"silent", @"connected", 15);
    NSDictionary *lost = waitEvent(@"silent", @"closed", 50);
    require([lost[@"reason"] isEqual:@"networkLost"], @"silent link did not fail via keepalives");
    puts("PASS native: silent link detected within 15s x 3");
    MFSSHTransport *untrusted = transport(fixture, @"none", nil);
    [untrusted open:@"untrusted" command:@"cat" fingerprint:nil]; waitEvent(@"untrusted", @"hostKey", 15);
    NSDictionary *untrustedClose = waitEvent(@"untrusted", @"closed", 65);
    require([untrustedClose[@"reason"] isEqual:@"hostKeyNotTrusted"], @"unanswered trust prompt had wrong reason");
    puts("PASS: native auth/trust, independent channels, binary writes, stderr, nullable exit status and cancellation");
  }
}

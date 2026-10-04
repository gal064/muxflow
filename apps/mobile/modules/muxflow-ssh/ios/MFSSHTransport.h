#import <Foundation/Foundation.h>

NS_ASSUME_NONNULL_BEGIN
typedef void (^MFSSHEvent)(NSDictionary<NSString *, id> *event);
typedef void (^MFSSHWriteCompletion)(NSString * _Nullable error);

// One native SSH session; the protocol and reconnect policy remain in TypeScript.
@interface MFSSHTransport : NSObject
@property(nonatomic, readonly) BOOL acceptingChannels;
- (nullable instancetype)initWithHost:(NSString *)host
                        port:(NSInteger)port
                        user:(NSString *)user
                  privatePem:(nullable NSString *)privatePem
                       event:(MFSSHEvent)event;
- (void)open:(NSString *)identifier command:(NSString *)command fingerprint:(nullable NSString *)fingerprint;
- (void)trust:(NSString *)identifier fingerprint:(NSString *)fingerprint;
- (void)write:(NSString *)identifier data:(NSData *)data completion:(MFSSHWriteCompletion)completion;
- (void)close:(NSString *)identifier;
@end
NS_ASSUME_NONNULL_END

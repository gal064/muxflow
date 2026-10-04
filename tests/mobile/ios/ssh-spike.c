// Small library-selection spike. No app logic or acceptance shortcuts live here.
#include <libssh2.h>
#include <arpa/inet.h>
#include <errno.h>
#include <time.h>
#include <netdb.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

static int partial_next_send;
static ssize_t send_bytes(libssh2_socket_t socket, const void *bytes, size_t length, int flags, void **abstract) {
  (void)abstract;
  if (partial_next_send) { partial_next_send = 0; length = 1; }
  ssize_t count = send(socket, bytes, length, flags);
  return count < 0 ? -errno : count;
}

// Deliberately vary the retry call stack; protocol packets must not live there.
__attribute__((noinline)) static int begin_close(LIBSSH2_CHANNEL *channel) {
  volatile char padding[1024]; padding[0] = 1;
  int result = muxflow_libssh2_channel_free_no_wait(channel);
  return result + padding[0] - 1;
}

static void require(int condition, const char *message) {
  if (!condition) { fprintf(stderr, "%s\n", message); exit(1); }
}

static int verify_hostkey(LIBSSH2_SESSION *session, const char *expected) {
  const unsigned char *hash = (const unsigned char *)libssh2_hostkey_hash(session, LIBSSH2_HOSTKEY_HASH_SHA256);
  if (!hash) return 0;
  char hex[65];
  for (int i = 0; i < 32; ++i) snprintf(hex + i * 2, 3, "%02x", hash[i]);
  return strcmp(hex, expected) == 0;
}

static char *read_key(const char *path, size_t *size) {
  FILE *file = fopen(path, "rb");
  require(file != NULL, "key file unavailable");
  require(fseek(file, 0, SEEK_END) == 0, "key size unavailable");
  long length = ftell(file);
  require(length > 0 && length < 16384, "unexpected key size");
  rewind(file);
  char *bytes = calloc((size_t)length + 1, 1);
  require(bytes != NULL && fread(bytes, 1, (size_t)length, file) == (size_t)length, "key read failed");
  fclose(file);
  *size = (size_t)length;
  return bytes;
}

int main(int argc, char **argv) {
  require(argc == 6, "usage: ssh-spike host port user host-key-sha256-hex key-pem-or-none");
  require(libssh2_init(0) == 0, "libssh2 init failed");
  struct addrinfo hints = {0}, *addresses = NULL;
  hints.ai_socktype = SOCK_STREAM;
  require(getaddrinfo(argv[1], argv[2], &hints, &addresses) == 0, "resolve failed");
  int fd = -1;
  for (struct addrinfo *a = addresses; a; a = a->ai_next) {
    fd = socket(a->ai_family, a->ai_socktype, a->ai_protocol);
    if (fd >= 0 && connect(fd, a->ai_addr, a->ai_addrlen) == 0) break;
    if (fd >= 0) close(fd);
    fd = -1;
  }
  freeaddrinfo(addresses);
  require(fd >= 0, "connect failed");
  LIBSSH2_SESSION *session = libssh2_session_init();
  require(session != NULL, "session unavailable");
  libssh2_session_callback_set2(session, LIBSSH2_CALLBACK_SEND, (libssh2_cb_generic *)send_bytes);
  libssh2_session_set_timeout(session, 10000);
  require(libssh2_session_handshake(session, fd) == 0, "handshake failed");
  require(verify_hostkey(session, argv[4]), "host key refused");
  require(muxflow_libssh2_session_pin_hostkey(session) == 0, "verified host key could not be pinned");
  char *methods = libssh2_userauth_list(session, argv[3], (unsigned int)strlen(argv[3]));
  if (!libssh2_userauth_authenticated(session)) {
    require(methods && strstr(methods, "publickey") && strcmp(argv[5], "none") != 0, "none refused and no publickey available");
    size_t size;
    char *key = read_key(argv[5], &size);
    int result = libssh2_userauth_publickey_frommemory(session, argv[3], strlen(argv[3]), NULL, 0, key, size, NULL);
    memset(key, 0, size);
    free(key);
    require(result == 0, "Ed25519 publickey authentication failed");
  }
  libssh2_keepalive_config(session, 1, 15);
  LIBSSH2_CHANNEL *first = libssh2_channel_open_session(session);
  LIBSSH2_CHANNEL *second = libssh2_channel_open_session(session);
  require(first && second, "two channels unavailable");
  require(libssh2_channel_exec(first, "printf 'one\\n'; printf 'diagnostic\\n' >&2; exit 17") == 0, "first exec failed");
  require(libssh2_channel_exec(second, "cat") == 0, "second exec failed");
  char bytes[64] = {0};
  ssize_t count = libssh2_channel_read(first, bytes, sizeof(bytes));
  require(count == 4 && memcmp(bytes, "one\n", 4) == 0, "stdout corrupt");
  count = libssh2_channel_read_stderr(first, bytes, sizeof(bytes));
  require(count == 11 && memcmp(bytes, "diagnostic\n", 11) == 0, "stderr unavailable");
  libssh2_channel_wait_eof(first);
  libssh2_channel_wait_closed(first);
  require(libssh2_channel_get_exit_status(first) == 17, "exit code unavailable");
  require(muxflow_libssh2_channel_has_exit_status(first), "exit status presence unavailable");
  libssh2_channel_free(first);
  require(libssh2_channel_write(second, "two\n", 4) == 4, "second channel was lost with first");
  count = libssh2_channel_read(second, bytes, sizeof(bytes));
  require(count == 4 && memcmp(bytes, "two\n", 4) == 0, "second stream corrupt");
  int next = 0;
  require(libssh2_keepalive_send(session, &next) == 0, "keepalive send failed");
  libssh2_channel_send_eof(second);
  libssh2_channel_wait_eof(second);
  libssh2_channel_wait_closed(second);
  require(muxflow_libssh2_channel_has_exit_status(second) && libssh2_channel_get_exit_status(second) == 0, "reported zero exit status lost");
  libssh2_channel_free(second);
  LIBSSH2_CHANNEL *third = libssh2_channel_open_session(session);
  require(third && libssh2_channel_exec(third, "no-status") == 0, "no-status channel failed");
  libssh2_channel_wait_eof(third);
  libssh2_channel_wait_closed(third);
  require(!muxflow_libssh2_channel_has_exit_status(third), "missing status invented as zero");
  libssh2_channel_free(third);
  LIBSSH2_CHANNEL *cancel = libssh2_channel_open_session(session);
  LIBSSH2_CHANNEL *sibling = libssh2_channel_open_session(session);
  require(cancel && sibling && libssh2_channel_exec(cancel, "cat") == 0 && libssh2_channel_exec(sibling, "cat") == 0, "cancel fixture unavailable");
  libssh2_session_set_blocking(session, 0);
  partial_next_send = 1;
  int close_result = begin_close(cancel);
  require(close_result == LIBSSH2_ERROR_EAGAIN, "partial EOF send was not exercised");
  for (int retry = 0; retry < 1000 && close_result == LIBSSH2_ERROR_EAGAIN; ++retry) {
    struct timespec pause = {0, 1000000}; nanosleep(&pause, NULL);
    close_result = muxflow_libssh2_channel_free_no_wait(cancel);
  }
  require(close_result == 0, "EOF retry header moved across call stacks");
  libssh2_session_set_blocking(session, 1);
  require(libssh2_channel_write(sibling, "sibling", 7) == 7, "sibling lost after partial EOF cancellation");
  count = libssh2_channel_read(sibling, bytes, sizeof(bytes));
  require(count == 7 && memcmp(bytes, "sibling", 7) == 0, "sibling response missing after cancellation");
  require(muxflow_libssh2_channel_free_no_wait(sibling) == 0, "sibling close failed");
  libssh2_session_disconnect(session, "spike complete");
  libssh2_session_free(session);
  close(fd);
  libssh2_exit();
  puts("PASS: auth, host-key callback, stderr/exit presence, independent exec channels, partial EOF cancellation, keepalive send");
}

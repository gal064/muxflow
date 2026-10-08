# Upstream get_exit_status cannot distinguish no status from reported zero.
# This presence bit leaves existing API behavior and SSH wire handling unchanged.
def replace_once(path, before, after)
  text = File.read(path)
  abort "libssh2 patch anchor changed: #{path}" unless text.scan(before).length == 1
  File.write(path, text.sub(before, after))
end

root = 'libssh2-1_11_1'
replace_once("#{root}/src/libssh2_priv.h", 'int exit_status;',
             'int exit_status; int muxflow_exit_status_received;')
replace_once("#{root}/src/packet.c", 'channelp->exit_status =',
             "channelp->muxflow_exit_status_received = 1;\n                        channelp->exit_status =")
declaration = 'LIBSSH2_API int muxflow_libssh2_channel_has_exit_status(LIBSSH2_CHANNEL *channel);'
replace_once("#{root}/include/libssh2.h",
             'LIBSSH2_API int libssh2_channel_get_exit_status(LIBSSH2_CHANNEL* channel);',
             "LIBSSH2_API int libssh2_channel_get_exit_status(LIBSSH2_CHANNEL* channel);\n#{declaration}")
File.open("#{root}/src/channel.c", 'a') do |file|
  file.write("\nLIBSSH2_API int muxflow_libssh2_channel_has_exit_status(LIBSSH2_CHANNEL *channel) {\n")
  file.write("    return channel && channel->muxflow_exit_status_received;\n}\n")
end

# Client cancellation sends EOF/close but does not wait for a remote close ack.
# Call only after the session's pending send operation has completed.
declaration = 'LIBSSH2_API int muxflow_libssh2_channel_free_no_wait(LIBSSH2_CHANNEL *channel);'
replace_once("#{root}/include/libssh2.h",
             'LIBSSH2_API int libssh2_channel_free(LIBSSH2_CHANNEL *channel);',
             "LIBSSH2_API int libssh2_channel_free(LIBSSH2_CHANNEL *channel);\n#{declaration}")
File.open("#{root}/src/channel.c", 'a') do |file|
  file.write("\nLIBSSH2_API int muxflow_libssh2_channel_free_no_wait(LIBSSH2_CHANNEL *channel) {\n")
  file.write("    if(channel) channel->remote.close = 1;\n")
  file.write("    return libssh2_channel_free(channel);\n}\n")
end

# Upstream keepalive_send hides EAGAIN and uses a stack packet. Retain that
# packet in the session and retry it before allowing another operation to send.
replace_once("#{root}/src/libssh2_priv.h", 'int keepalive_interval;',
             'unsigned char muxflow_keepalive_data[27]; int keepalive_interval;')
replace_once("#{root}/src/keepalive.c", 'unsigned char keepalive_data[]',
             'static const unsigned char initial_keepalive_data[]')
replace_once("#{root}/src/keepalive.c", 'size_t len = sizeof(keepalive_data) - 1;',
             "size_t len = sizeof(initial_keepalive_data) - 1;\n        unsigned char *keepalive_data = session->muxflow_keepalive_data;")
replace_once("#{root}/src/keepalive.c", '        int rc;',
             "        int rc;\n        memcpy(keepalive_data, initial_keepalive_data, len);")
replace_once("#{root}/src/keepalive.c", 'if(rc && rc != LIBSSH2_ERROR_EAGAIN) {',
             "if(rc == LIBSSH2_ERROR_EAGAIN) return rc;\n        if(rc) {")

# The application pins the verified host key before authentication. Automatic
# rekey must enforce that pin inside kex, before any further application bytes.
replace_once("#{root}/src/libssh2_priv.h", 'int keepalive_interval;',
             'unsigned char muxflow_hostkey_pin[32]; int muxflow_hostkey_pinned; int muxflow_hostkey_mismatch; int keepalive_interval;')
declarations = "LIBSSH2_API int muxflow_libssh2_session_pin_hostkey(LIBSSH2_SESSION *session);\n" +
               'LIBSSH2_API int muxflow_libssh2_session_hostkey_mismatch(LIBSSH2_SESSION *session);'
replace_once("#{root}/include/libssh2.h", '#define LIBSSH2_HOSTKEY_HASH_SHA256                         3',
             "#define LIBSSH2_HOSTKEY_HASH_SHA256                         3\n#{declarations}")
replace_once("#{root}/src/kex.c", '    /* Done with kexinit buffers */',
             "    if(!rc && session->muxflow_hostkey_pinned &&\n" +
             "       (!session->server_hostkey_sha256_valid ||\n" +
             "        memcmp(session->muxflow_hostkey_pin, session->server_hostkey_sha256, 32))) {\n" +
             "        session->muxflow_hostkey_mismatch = 1;\n" +
             "        rc = _libssh2_error(session, LIBSSH2_ERROR_HOSTKEY_SIGN, \"Host key changed during rekey\");\n" +
             "    }\n\n    /* Done with kexinit buffers */")
File.open("#{root}/src/session.c", 'a') do |file|
  file.write("\nLIBSSH2_API int muxflow_libssh2_session_pin_hostkey(LIBSSH2_SESSION *session) {\n")
  file.write("    if(!session || !session->server_hostkey_sha256_valid) return -1;\n")
  file.write("    memcpy(session->muxflow_hostkey_pin, session->server_hostkey_sha256, 32);\n")
  file.write("    session->muxflow_hostkey_pinned = 1;\n    return 0;\n}\n")
  file.write("\nLIBSSH2_API int muxflow_libssh2_session_hostkey_mismatch(LIBSSH2_SESSION *session) {\n")
  file.write("    return session && session->muxflow_hostkey_mismatch;\n}\n")
end

# Retry ownership alone is insufficient when upstream packet headers live on
# the stack. Keep EOF and channel-request replies at stable native addresses.
replace_once("#{root}/src/libssh2_priv.h", 'unsigned char close_packet[5];',
             'unsigned char muxflow_eof_packet[5]; unsigned char close_packet[5];')
replace_once("#{root}/src/channel.c",
             'unsigned char packet[5];    /* packet_type(1) + channelno(4) */',
             'unsigned char *packet = channel->muxflow_eof_packet;')
replace_once("#{root}/src/libssh2_priv.h", 'int keepalive_interval;',
             'unsigned char muxflow_channel_reply[5]; int keepalive_interval;')
replace_once("#{root}/src/packet.c", 'unsigned char packet[5];',
             'unsigned char *packet;')
replace_once("#{root}/src/packet.c", 'libssh2_packet_add_jump_point4:',
             "libssh2_packet_add_jump_point4:\n                    packet = session->muxflow_channel_reply;")

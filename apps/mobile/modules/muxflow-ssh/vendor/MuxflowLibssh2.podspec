# Based on apotocki/libssh2-iosx 1.11.1.0 (BSD-3-Clause).
# Retain the upstream build; patch contract gaps in cancellation, status and pins.
patch = File.read(File.join(__dir__, 'patch-libssh2.rb'))
Pod::Spec.new do |s|
  s.name = 'MuxflowLibssh2'
  s.version = '1.11.1.0'
  s.summary = 'Pinned libssh2 Apple build for the native Muxflow SSH contract'
  s.homepage = 'https://github.com/apotocki/libssh2-iosx'
  s.license = { :type => 'BSD-3-Clause' }
  s.author = 'Alexander Pototskiy and libssh2 contributors'
  s.source = { :git => 'https://github.com/apotocki/libssh2-iosx.git', :tag => '1.11.1.0' }
  s.platform = :ios, '16.4'
  s.static_framework = true
  s.requires_arc = false
  s.header_mappings_dir = 'frameworks/Headers'
  s.public_header_files = 'frameworks/Headers/**/*.{h,H}'
  s.source_files = 'frameworks/Headers/**/*.{h,H}'
  s.vendored_frameworks = 'frameworks/ssh2.xcframework'
  s.libraries = 'z'
  s.dependency 'openssl-iosx', '3.5.9.1'
  s.prepare_command = <<-SHELL
    set -eu
    git clone --depth 1 --branch libssh2-1.11.1 https://github.com/libssh2/libssh2.git libssh2-1_11_1
    ruby <<'RUBY'
#{patch}
RUBY
    sh scripts/build.sh --platforms=ios,iossim
  SHELL
end

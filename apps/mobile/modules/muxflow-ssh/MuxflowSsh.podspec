Pod::Spec.new do |s|
  s.name = 'MuxflowSsh'
  s.version = '0.1.0'
  s.summary = 'Native SSH byte transport for Muxflow mobile'
  s.description = s.summary
  s.license = { :type => 'MIT' }
  s.author = 'Muxflow'
  s.homepage = 'https://github.com/gal064/muxflow'
  s.source = { :git => 'https://github.com/gal064/muxflow.git' }
  s.platform = :ios, '16.4'
  s.swift_version = '5.9'
  s.static_framework = true
  s.source_files = 'ios/**/*.{h,m,swift}'
  s.public_header_files = 'ios/MFSSHTransport.h'
  s.dependency 'ExpoModulesCore'
  s.dependency 'MuxflowLibssh2', '1.11.1.0'
  s.frameworks = 'CryptoKit', 'Security'
  s.pod_target_xcconfig = { 'DEFINES_MODULE' => 'YES' }
end

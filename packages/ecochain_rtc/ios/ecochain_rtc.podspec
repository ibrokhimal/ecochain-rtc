#
# To learn more about a Podspec see http://guides.cocoapods.org/syntax/podspec.html.
# Run `pod lib lint ecochain_rtc.podspec` to validate before publishing.
#
Pod::Spec.new do |s|
  s.name             = 'ecochain_rtc'
  s.version          = '0.1.0'
  s.summary          = 'Ecochain RTC SDK'
  s.description      = <<-DESC
Reports screen lock and phone calls to the Ecochain RTC SDK.
                       DESC
  s.homepage         = 'https://ecochain.uz'
  s.license          = { :type => 'Proprietary' }
  s.author           = 'Ecochain'
  s.source           = { :path => '.' }
  s.source_files = 'ecochain_rtc/Sources/ecochain_rtc/**/*'
  s.dependency 'Flutter'
  s.dependency 'WebRTC-SDK'
  s.frameworks = 'CallKit'
  s.platform = :ios, '13.0'

  # Flutter.framework does not contain a i386 slice.
  s.pod_target_xcconfig = { 'DEFINES_MODULE' => 'YES', 'EXCLUDED_ARCHS[sdk=iphonesimulator*]' => 'i386' }
  s.swift_version = '5.0'

end

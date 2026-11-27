import 'package:ecochain_rtc/src/internal/video_codec.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:flutter/services.dart';
import 'package:flutter_webrtc/flutter_webrtc.dart';

RTCRtpCodecCapability codec(String name, [String? fmtp]) =>
    RTCRtpCodecCapability(clockRate: 90000, mimeType: 'video/$name', sdpFmtpLine: fmtp);

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  test('prefers fragmented H264 baseline and keeps fallback and repair capabilities intact', () {
    final vp8 = codec('VP8');
    final rtx = codec('rtx', 'apt=96');
    final high = codec('H264', 'profile-level-id=640c1f;packetization-mode=1');
    final mode0 = codec('H264', 'profile-level-id=42e01f;packetization-mode=0');
    final baseline = codec('h264', ' packetization-mode = 1 ; profile-level-id = 42E01F ');
    final red = codec('red');
    final fec = codec('ulpfec');
    final source = [vp8, rtx, high, mode0, baseline, red, fec];
    final ordered = preferredVideoCodecs(source);
    expect(ordered, [baseline, high, vp8, rtx, mode0, red, fec]);
    expect(source, [vp8, rtx, high, mode0, baseline, red, fec]);
    expect(ordered[0].sdpFmtpLine, ' packetization-mode = 1 ; profile-level-id = 42E01F ');
  });

  test('prefers VP8 when packetization-mode 1 H264 is unavailable', () {
    final av1 = codec('AV1');
    final missingMode = codec('H264', 'profile-level-id=42e01f');
    final vp8 = codec('VP8');
    expect(preferredVideoCodecs([av1, missingMode, vp8]), [vp8, av1, missingMode]);
    expect(preferredVideoCodecs([]), isEmpty);
  });

  test('preserves capability order within each preference group', () {
    final a = codec('H264', 'packetization-mode=1;profile-level-id=42e01f');
    final b = codec('H264', 'packetization-mode=1;profile-level-id=42e033');
    final vp8 = codec('VP8');
    expect(preferredVideoCodecs([vp8, a, b]), [a, b, vp8]);
  });

  group('native capability boundary', () {
    var failRead = false;
    setUp(() {
      failRead = false;
      TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger.setMockMethodCallHandler(
        const MethodChannel('FlutterWebRTC.Method'),
        (call) async {
          if (call.method == 'initialize') return null;
          if (call.method != 'getRtpSenderCapabilities') throw StateError('unexpected ${call.method}');
          expect((call.arguments as Map)['kind'], 'video');
          if (failRead) throw PlatformException(code: 'unsupported');
          return {
            'codecs': [
              codec('VP8').toMap(),
              codec('H264', 'packetization-mode=1;profile-level-id=42e01f').toMap(),
              codec('rtx').toMap(),
            ],
            'headerExtensions': <Object>[],
            'fecMechanisms': <Object>[],
          };
        },
      );
    });
    tearDown(() {
      TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger.setMockMethodCallHandler(
        const MethodChannel('FlutterWebRTC.Method'),
        null,
      );
    });

    test('applies discovered H264 preferences before negotiation', () async {
      final transceiver = _Transceiver();
      await configureVideoCodecs(transceiver);
      expect(transceiver.preferences!.map((c) => c.mimeType), ['video/H264', 'video/VP8', 'video/rtx']);
    });

    test('unsupported capability API keeps platform defaults usable', () async {
      failRead = true;
      final transceiver = _Transceiver();
      await configureVideoCodecs(transceiver);
      expect(transceiver.preferences, isNull);
    });

    test('rejected preferences do not prevent camera publication', () async {
      final transceiver = _Transceiver()..reject = true;
      await expectLater(configureVideoCodecs(transceiver), completes);
      expect(transceiver.preferences, isNotNull);
    });
  });
}

class _Transceiver extends Fake implements RTCRtpTransceiver {
  List<RTCRtpCodecCapability>? preferences;
  bool reject = false;

  @override
  Future<void> setCodecPreferences(List<RTCRtpCodecCapability> codecs) async {
    preferences = codecs;
    if (reject) throw PlatformException(code: 'unsupported');
  }
}

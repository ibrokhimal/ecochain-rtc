// Native camera -> local SFU -> native decoder. Requires camera permission.
// Run a development server, then:
// flutter test integration_test/h264_simulcast_test.dart -d macos
// For a phone, set --dart-define=ECOCHAIN_RTC_URL=ws://<LAN-IP>:7880/v1/rtc.
// ignore_for_file: avoid_print
import 'package:ecochain_meeting_ui/ecochain_meeting_ui.dart';
import 'package:ecochain_rtc/ecochain_rtc.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';

const _url = String.fromEnvironment('ECOCHAIN_RTC_URL', defaultValue: 'ws://127.0.0.1:7880/v1/rtc');

void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();

  testWidgets('native H264 camera decodes at all three simulcast sizes through SFU', (tester) async {
    final publisher = RtcSession();
    final receiver = RtcSession();
    try {
      final room = 'h264-${DateTime.now().millisecondsSinceEpoch}';
      Future<void> connect(RtcSession session, String identity, {bool camera = false}) async {
        final url = Uri.parse(_url);
        final token = await fetchDevToken(url, room: room, identity: identity, name: identity);
        print('CONNECT $identity');
        await session
            .connect(RtcConnectOptions(url: url, token: token, camera: camera, allowInsecureLocalhost: true))
            .timeout(const Duration(seconds: 30), onTimeout: () => throw StateError('$identity connect timed out'));
        print('CONNECTED $identity');
      }

      await connect(publisher, 'camera');
      print('OPEN CAMERA');
      await publisher
          .setCameraEnabled(true)
          .timeout(
            const Duration(seconds: 30),
            onTimeout: () =>
                throw StateError('native camera publication timed out; check OS permission and WebRTC logs'),
          );
      print('CAMERA PUBLISHED');
      await connect(receiver, 'receiver');
      final publisherId = publisher.localParticipant!.id;
      var previousWidth = 0;
      for (final quality in ['low', 'medium', 'high']) {
        receiver.setVideoLayout(
          visibleParticipantIds: [
            publisherId,
            if (quality == 'low') ...['unused-1', 'unused-2', 'unused-3', 'unused-4'],
          ],
          pinnedParticipantId: quality == 'high' ? publisherId : null,
        );
        final baseline = (await receiver.getReceiveStats().timeout(const Duration(seconds: 5))).videoFramesDecoded;
        print('WAIT $quality after decoded=$baseline');
        final deadline = DateTime.now().add(const Duration(seconds: 35));
        var matched = false;
        while (DateTime.now().isBefore(deadline)) {
          await Future<void>.delayed(const Duration(milliseconds: 300));
          final track = receiver.remoteParticipants.where((p) => p.id == publisherId).firstOrNull?.camera;
          final reports = await track?.readStats?.call().timeout(const Duration(seconds: 5));
          if (reports == null) continue;
          final codecs = {for (final r in reports.where((r) => r.type == 'codec')) r.id: r.values['mimeType']};
          for (final report in reports.where((r) => r.type == 'inbound-rtp')) {
            final values = report.values;
            final width = (values['frameWidth'] as num?)?.toInt() ?? 0;
            final height = (values['frameHeight'] as num?)?.toInt() ?? 0;
            final frames = (values['framesDecoded'] as num?)?.toInt() ?? 0;
            if (width <= previousWidth || frames < baseline + 10) continue;
            // A stale larger frame must not satisfy the low-layer check.
            if (quality == 'low' && width > 384) continue;
            expect(codecs[values['codecId']], 'video/H264');
            expect(track!.simulcast, isTrue);
            print('H264 $quality: ${width}x$height decoded=$frames');
            previousWidth = width;
            matched = true;
            break;
          }
          if (matched) break;
        }
        expect(matched, isTrue, reason: 'no newly decoded H264 $quality layer after width=$previousWidth');
      }
    } finally {
      await receiver.leave().timeout(const Duration(seconds: 5));
      await publisher.leave().timeout(const Duration(seconds: 5));
      receiver.dispose();
      publisher.dispose();
    }
  }, timeout: const Timeout(Duration(minutes: 3)));
}

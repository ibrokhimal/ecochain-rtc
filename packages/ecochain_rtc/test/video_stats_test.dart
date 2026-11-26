import 'package:ecochain_rtc/src/internal/video_stats.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:flutter_webrtc/flutter_webrtc.dart';

StatsReport report(String id, String type, Map<String, Object?> values) => StatsReport(id, type, 0, values);

List<StatsReport> video({String id = 'in', int bytes = 1000, int frames = 10, Map<String, Object?> extra = const {}}) =>
    [
      report('codec', 'codec', {'mimeType': 'video/VP8'}),
      report(id, 'inbound-rtp', {
        'kind': 'video',
        'codecId': 'codec',
        'trackIdentifier': 'native-video',
        'frameWidth': 640,
        'frameHeight': 360,
        'bytesReceived': bytes,
        'framesDecoded': frames,
        ...extra,
      }),
    ];

void main() {
  test('reads codec and dimensions and derives current FPS and bitrate', () {
    final sampler = VideoStatsSampler();
    final first = sampler.sample(video(), at: Duration.zero, mediaTrackId: 'native-video')!;
    expect((first.codec, first.width, first.height), ('VP8', 640, 360));
    expect(first.kbps, isNull);
    expect(first.fps, isNull);
    final next = sampler.sample(
      video(bytes: 61000, frames: 35),
      at: const Duration(seconds: 1),
      mediaTrackId: 'native-video',
    )!;
    expect(next.kbps, 480);
    expect(next.fps, 25);
    final paused = sampler.sample(
      video(bytes: 61000, frames: 35, extra: {'framesPerSecond': 25}),
      at: const Duration(seconds: 2),
      mediaTrackId: 'native-video',
    )!;
    expect(paused.kbps, 0);
    expect(paused.fps, 0, reason: 'a stale reported FPS must not hide a stopped stream');
  });

  test('never combines other participants, audio or repair streams', () {
    final sample = VideoStatsSampler().sample(
      [
        ...video(),
        report('other', 'inbound-rtp', {
          'kind': 'video',
          'trackIdentifier': 'other-video',
          'frameWidth': 1920,
          'bytesReceived': 999999,
        }),
        report('audio', 'inbound-rtp', {'kind': 'audio', 'bytesReceived': 999999}),
        report('rtx-codec', 'codec', {'mimeType': 'video/rtx'}),
        report('rtx', 'inbound-rtp', {'kind': 'video', 'codecId': 'rtx-codec', 'bytesReceived': 999999}),
      ],
      at: Duration.zero,
      mediaTrackId: 'native-video',
    )!;
    expect(sample.width, 640);
    expect(sample.codec, 'VP8');
  });

  test('counter reset and new report do not create negative or huge rates', () {
    final sampler = VideoStatsSampler();
    sampler.sample(video(bytes: 100000, frames: 100), at: Duration.zero);
    final reset = sampler.sample(video(bytes: 0, frames: 0), at: const Duration(seconds: 1))!;
    expect(reset.kbps, isNull);
    expect(reset.fps, isNull);
    final replacement = sampler.sample(video(id: 'new', bytes: 900000, frames: 1000), at: const Duration(seconds: 2))!;
    expect(replacement.kbps, isNull);
    expect(replacement.fps, isNull);
  });

  test('missing fields remain unknown and unavailable stats reset the baseline', () {
    final sampler = VideoStatsSampler();
    final sample = sampler.sample([
      report('in', 'inbound-rtp', {'mediaType': 'video'}),
    ], at: Duration.zero)!;
    expect((sample.codec, sample.width, sample.height, sample.fps, sample.kbps), (null, null, null, null, null));
    expect(sampler.sample(video(), at: const Duration(seconds: 1)), isNotNull);
    expect(sampler.sample([], at: const Duration(seconds: 2)), isNull);
    expect(sampler.sample(video(bytes: 999999), at: const Duration(seconds: 3))!.kbps, isNull);
  });

  test('resolves legacy track references and refuses ambiguous unmatched video', () {
    final sampler = VideoStatsSampler();
    final reports = [
      report('track-ref', 'track', {'trackIdentifier': 'native-video'}),
      report('in', 'inbound-rtp', {
        'mediaType': 'video',
        'trackId': 'track-ref',
        'frameWidth': '320',
        'frameHeight': '180',
      }),
    ];
    expect(sampler.sample(reports, at: Duration.zero, mediaTrackId: 'native-video')!.width, 320);
    expect(sampler.sample(video(), at: Duration.zero, mediaTrackId: 'wrong-video'), isNull);
    expect(
      sampler.sample(
        [
          report('other', 'inbound-rtp', {'kind': 'video', 'trackIdentifier': 'other-video'}),
          report('unknown', 'inbound-rtp', {'kind': 'video'}),
        ],
        at: Duration.zero,
        mediaTrackId: 'native-video',
      ),
      isNull,
    );
    expect(
      sampler.sample([
        report('a', 'inbound-rtp', {'kind': 'video'}),
        report('b', 'inbound-rtp', {'kind': 'video'}),
      ], at: Duration.zero),
      isNull,
    );
  });
}

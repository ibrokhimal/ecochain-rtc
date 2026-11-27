import 'package:flutter/foundation.dart';
import 'package:flutter_webrtc/flutter_webrtc.dart';

/// Reorder capabilities, without inventing codecs or changing native fmtp.
/// Mode 1 supports fragmented NAL units. Prefer constrained baseline for
/// interoperability, then other mode-1 H264 profiles, then VP8 as fallback.
/// Keep every remaining capability, including RTX/RED/FEC, in its original order.
List<RTCRtpCodecCapability> preferredVideoCodecs(List<RTCRtpCodecCapability> codecs) {
  final groups = List.generate(4, (_) => <RTCRtpCodecCapability>[]);
  for (final codec in codecs) {
    final mime = codec.mimeType.toLowerCase();
    var priority = mime == 'video/vp8' ? 2 : 3;
    if (mime == 'video/h264') {
      final fmtp = <String, String>{};
      for (final parameter in (codec.sdpFmtpLine ?? '').split(';')) {
        final separator = parameter.indexOf('=');
        if (separator < 0) continue;
        fmtp[parameter.substring(0, separator).trim().toLowerCase()] = parameter
            .substring(separator + 1)
            .trim()
            .toLowerCase();
      }
      if (fmtp['packetization-mode'] == '1') {
        priority = (fmtp['profile-level-id'] ?? '').startsWith('42e0') ? 0 : 1;
      }
    }
    groups[priority].add(codec);
  }
  return [for (final group in groups) ...group];
}

/// Called before the publisher offer, including after reconnect. Older
/// platforms may lack this API; a preference failure must not stop capture.
Future<void> configureVideoCodecs(RTCRtpTransceiver transceiver) async {
  try {
    final capabilities = await getRtpSenderCapabilities('video');
    final codecs = preferredVideoCodecs(capabilities.codecs ?? []);
    if (codecs.isNotEmpty) await transceiver.setCodecPreferences(codecs);
  } catch (error) {
    if (kDebugMode) debugPrint('Video codec preference unavailable; using WebRTC defaults: $error');
  }
}

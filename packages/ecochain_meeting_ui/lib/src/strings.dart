import 'package:ecochain_rtc/ecochain_rtc.dart';

/// User facing texts (Uzbek). Kept in one place so a localization package
/// can replace them later without touching widgets.
abstract final class MeetingStrings {
  /// Join button.
  static const join = 'Qo‘shilish';

  /// Leave button.
  static const leave = 'Chiqish';

  /// End meeting button (host).
  static const endForAll = 'Hamma uchun yakunlash';

  /// Microphone label.
  static const microphone = 'Mikrofon';

  /// Camera label.
  static const camera = 'Kamera';

  /// Switch camera tooltip.
  static const switchCamera = 'Kamerani almashtirish';

  /// Screen share label.
  static const screen = 'Ekran';

  /// Participants label.
  static const participants = 'Ishtirokchilar';

  /// Local participant suffix.
  static const you = '(siz)';

  /// Server URL field.
  static const serverUrl = 'Server manzili';

  /// Token field.
  static const token = 'Xona tokeni';

  /// Dev token button.
  static const devToken = 'Development token olish';

  /// Room field.
  static const room = 'Xona';

  /// Name field.
  static const name = 'Ism';

  /// Host checkbox.
  static const host = 'Mezbon sifatida';

  /// Status line for a state.
  static String state(RtcConnectionState state) => switch (state) {
    RtcConnectionState.idle => 'Ulanmagan',
    RtcConnectionState.connecting => 'Ulanmoqda…',
    RtcConnectionState.connected => 'Uchrashuv faol',
    RtcConnectionState.reconnecting => 'Aloqa tiklanmoqda…',
    RtcConnectionState.disconnected => 'Uchrashuv tugadi',
  };

  /// Explanation of why a session ended.
  static String reason(RtcDisconnectReason? reason) => switch (reason) {
    RtcDisconnectReason.left || null => 'Siz uchrashuvdan chiqdingiz.',
    RtcDisconnectReason.roomEnded => 'Mezbon uchrashuvni yakunladi.',
    RtcDisconnectReason.removed => 'Mezbon sizni uchrashuvdan chiqardi.',
    RtcDisconnectReason.replacedByOtherSession => 'Shu hisob bilan boshqa qurilmadan ulanildi.',
    RtcDisconnectReason.unauthorized => 'Ruxsat yo‘q yoki token muddati tugagan.',
    RtcDisconnectReason.roomFull => 'Xona to‘lgan.',
    RtcDisconnectReason.roomNotFound => 'Xona topilmadi.',
    RtcDisconnectReason.networkFailed => 'Tarmoq orqali ulanib bo‘lmadi.',
    RtcDisconnectReason.error => 'Kutilmagan xato yuz berdi.',
  };

  /// Message for an error code.
  static String error(RtcErrorCode code) => switch (code) {
    RtcErrorCode.permissionDenied => 'Mikrofon yoki kameraga ruxsat berilmagan.',
    RtcErrorCode.deviceError => 'Kamera yoki mikrofonni ochib bo‘lmadi (band yoki mavjud emas).',
    RtcErrorCode.invalidUrl => 'Server manzili noto‘g‘ri. wss:// manzil kiriting.',
    RtcErrorCode.roomFull => 'Xona to‘lgan.',
    RtcErrorCode.roomEnded => 'Uchrashuv yakunlangan.',
    RtcErrorCode.unauthorized || RtcErrorCode.tokenExpired => 'Token yaroqsiz yoki muddati tugagan.',
    RtcErrorCode.forbidden => 'Bu amal uchun ruxsat yo‘q.',
    RtcErrorCode.connectionLost || RtcErrorCode.timeout => 'Serverga ulanib bo‘lmadi.',
    _ => 'Xato: ${code.name}',
  };
}

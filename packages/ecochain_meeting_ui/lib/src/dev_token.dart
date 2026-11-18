import 'dart:convert';
import 'dart:io';

/// Fetches a token from the development-only `POST /v1/dev/token` endpoint.
///
/// [signalingUrl] is the WebSocket URL the user typed (`ws://host:7880/v1/rtc`);
/// the HTTP origin is derived from it. Production servers answer 404, which
/// throws a [HttpException]: real apps get tokens from their own backend.
///
/// ```dart
/// final token = await fetchDevToken(
///   Uri.parse('ws://192.168.1.10:7880/v1/rtc'),
///   room: 'demo', identity: 'alice', name: 'Alice', host: true,
/// );
/// ```
Future<String> fetchDevToken(
  Uri signalingUrl, {
  required String room,
  required String identity,
  String? name,
  bool host = false,
}) async {
  final http = signalingUrl.replace(scheme: signalingUrl.scheme == 'wss' ? 'https' : 'http', path: '/v1/dev/token');
  final client = HttpClient()..connectionTimeout = const Duration(seconds: 5);
  try {
    final request = await client.postUrl(http);
    request.headers.contentType = ContentType.json;
    request.write(
      jsonEncode({'room': room, 'identity': identity, 'name': ?name, 'role': host ? 'host' : 'participant'}),
    );
    final response = await request.close();
    final body = await utf8.decodeStream(response);
    if (response.statusCode != 200) {
      throw HttpException('dev token endpoint answered ${response.statusCode}', uri: http);
    }
    return (jsonDecode(body) as Map<String, dynamic>)['token'] as String;
  } finally {
    client.close(force: true);
  }
}

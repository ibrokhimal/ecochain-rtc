import 'dart:async';
import 'dart:convert';
import 'dart:io';

/// A tiny WebSocket server speaking just enough of the protocol for tests.
class FakeServer {
  FakeServer._(this._server);

  final HttpServer _server;
  final sockets = <WebSocket>[];
  final received = <Map<String, dynamic>>[];

  /// Called for every request; return `null` to not answer.
  FutureOr<Map<String, dynamic>?> Function(Map<String, dynamic> request, WebSocket socket) onRequest =
      (request, socket) => ok(request, {});

  Uri get url => Uri.parse('ws://127.0.0.1:${_server.port}/v1/rtc');

  static Future<FakeServer> start() async {
    final server = FakeServer._(await HttpServer.bind(InternetAddress.loopbackIPv4, 0));
    server._server.listen((request) async {
      final socket = await WebSocketTransformer.upgrade(request);
      server.sockets.add(socket);
      socket.listen((frame) async {
        final message = jsonDecode(frame as String) as Map<String, dynamic>;
        server.received.add(message);
        final reply = await server.onRequest(message, socket);
        if (reply != null) socket.add(jsonEncode(reply));
      });
    });
    return server;
  }

  static Map<String, dynamic> ok(Map<String, dynamic> request, Map<String, dynamic> data) => {
    'v': 1,
    'type': 'response',
    'id': request['id'],
    'ok': true,
    'data': data,
  };

  static Map<String, dynamic> error(Map<String, dynamic> request, String code) => {
    'v': 1,
    'type': 'response',
    'id': request['id'],
    'ok': false,
    'error': {'code': code, 'message': code, 'retryable': false},
  };

  void push(String type, Map<String, dynamic> data) {
    for (final socket in sockets) {
      socket.add(jsonEncode({'v': 1, 'type': type, 'data': data}));
    }
  }

  Future<void> close() async {
    for (final socket in sockets) {
      await socket.close();
    }
    await _server.close(force: true);
  }
}

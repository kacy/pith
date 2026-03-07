# Simple Chat Server

A basic WebSocket chat server demonstrating the http and websocket libraries.

## Quick Start

```bash
# Run the server
./self-host/forge_main run examples/chat_simple.fg

# Or build and run
./self-host/forge_main build examples/chat_simple.fg
./examples/chat_simple
```

Then open `client.html` in your browser.

## Usage

1. Start the server (runs on port 8080)
2. Open multiple browser tabs to `http://localhost:8080`
3. Type messages and see them broadcast to all connected clients

## Features

- WebSocket protocol for real-time communication
- Broadcasts messages to all connected clients
- Simple echo for ping/pong keepalive
- Graceful client disconnect handling

## Protocol

### Client → Server
```json
{"text": "Hello everyone!"}
```

### Server → Client
```json
{"text": "Hello everyone!", "time": 1234567890}
```

## Files

- `main.fg` - Server implementation
- `client.html` - Web client

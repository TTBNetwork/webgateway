#!/usr/bin/env node
// MCSManager WebSocket probe.
//
// Connects to the daemon's Socket.IO endpoint (optionally through the gateway),
// authenticates with the daemon key, calls the read-only "info/overview" event
// and then keeps the connection alive long enough to cross an engine.io ping
// interval (20s by default). It exits 0 only when the engine.io handshake, the
// Socket.IO namespace connect, the daemon auth and a real request all succeed.
//
// Usage:
//   WS_URL='ws://127.0.0.2:18080/socket.io/?EIO=4&transport=websocket' \
//   MCS_KEY='<daemon key>' HOLD_SECONDS=45 node mcsmanager_ws_probe.mjs
//
// For a wss upstream behind a self-signed test certificate:
//   NODE_TLS_REJECT_UNAUTHORIZED=0 WS_URL='wss://...' ...
//
// No npm dependencies: Node >= 22 exposes a global WebSocket.

const url =
  process.env.WS_URL ?? "ws://127.0.0.2:18080/socket.io/?EIO=4&transport=websocket";
const key = process.env.MCS_KEY;
const holdSeconds = Number(process.env.HOLD_SECONDS ?? "45");
const log = (...args) => console.log("[mcs-probe]", ...args);

if (!key) {
  console.error("MCS_KEY is required (MCSManager daemon key)");
  process.exit(2);
}

const state = {
  engineOpen: false,
  socketConnected: false,
  authenticated: false,
  overview: false,
  ping: false,
  closedByRemote: false,
};

let finished = false;

function finish(code, reason) {
  if (finished) return;
  finished = true;
  clearTimeout(holdTimer);
  try {
    ws.close();
  } catch {}
  if (code === 0) {
    log("PASS", JSON.stringify(state));
    process.exit(0);
  }
  log("FAIL", reason, JSON.stringify(state));
  process.exit(code);
}

const ws = new WebSocket(url);

const deadline = setTimeout(
  () => finish(1, "timed out waiting for the WebSocket probe"),
  (holdSeconds + 20) * 1000,
);

let holdTimer;

ws.addEventListener("open", () => log("websocket open", url));

ws.addEventListener("message", (event) => {
  const data = typeof event.data === "string" ? event.data : String(event.data);

  // Engine.IO open packet -> open the default Socket.IO namespace.
  if (data.startsWith("0{")) {
    state.engineOpen = true;
    const info = JSON.parse(data.slice(1));
    log("engine.io open", { pingInterval: info.pingInterval, pingTimeout: info.pingTimeout });
    ws.send("40");
    return;
  }

  // Socket.IO namespace connected -> authenticate.
  if (data.startsWith("40")) {
    state.socketConnected = true;
    log("socket.io namespace connected");
    ws.send("42" + JSON.stringify(["auth", { uuid: null, data: key }]));
    return;
  }

  // Engine.IO ping -> pong.
  if (data === "2") {
    state.ping = true;
    log("engine.io ping received, sending pong");
    ws.send("3");
    return;
  }

  if (data.startsWith("42")) {
    const [name, packet] = JSON.parse(data.slice(2));
    if (name === "auth") {
      if (packet?.data !== true) {
        finish(1, `daemon rejected the key: ${JSON.stringify(packet)}`);
        return;
      }
      state.authenticated = true;
      log("daemon auth accepted");
      ws.send("42" + JSON.stringify(["info/overview", { uuid: null, data: null }]));
      return;
    }

    if (name === "info/overview") {
      if (packet?.status !== 200) {
        finish(1, `info/overview failed: ${JSON.stringify(packet)}`);
        return;
      }
      state.overview = true;
      log("info/overview", {
        version: packet.data?.version,
        instances: packet.data?.instance,
        platform: packet.data?.system?.platform,
      });
      log(`holding the connection for ${holdSeconds}s`);
      holdTimer = setTimeout(() => {
        if (!state.ping) log("warning: no engine.io ping seen within the hold window");
        finish(0);
      }, holdSeconds * 1000);
      return;
    }

    log("unhandled socket.io event", name);
    return;
  }

  if (data === "1" || data === "41") {
    state.closedByRemote = true;
    finish(1, `remote closed the socket: ${data}`);
  }
});

ws.addEventListener("close", (event) => {
  if (!finished) {
    finish(1, `socket closed early (code ${event.code})`);
  }
});

ws.addEventListener("error", (event) => {
  finish(1, `websocket error: ${event.message ?? "unknown"}`);
});

// Keep the process alive until finish() or the deadline fires.
process.on("SIGINT", () => finish(130, "interrupted"));

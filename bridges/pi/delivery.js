import net from "node:net";
import { chmod, unlink } from "node:fs/promises";

// One private socket per launch. It is published only after MCP discovery.
export async function startDelivery(path, pi, ctx) {
  if (!path) return undefined;
  const clients = new Set();
  const server = net.createServer((socket) => {
    clients.add(socket);
    socket.on("close", () => clients.delete(socket));
    socket.on("error", () => {});
    socket.setTimeout(3000, () => socket.destroy());
    socket.setEncoding("utf8");
    let input = "";
    socket.on("data", (chunk) => {
      input += chunk;
      if (input.length > 1024 * 1024) return socket.destroy();
      if (!input.includes("\n")) return;
      socket.removeAllListeners("data");
      try {
        const message = JSON.parse(input.slice(0, input.indexOf("\n")));
        if (message.session === true) {
          socket.end(JSON.stringify({ session: ctx.sessionManager.getSessionFile() }) + "\n");
        } else {
          if (typeof message.text !== "string" || !message.text) throw new Error("missing text");
          pi.sendMessage({ customType: "omar_event", content: message.text, display: true },
            { triggerTurn: true, deliverAs: "followUp" });
          socket.end('{"accepted":true}\n');
        }
      } catch (error) {
        socket.end(JSON.stringify({ error: String(error) }) + "\n");
      }
    });
  });
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(path, resolve);
  });
  try {
    await chmod(path, 0o600);
  } catch (error) {
    for (const client of clients) client.destroy();
    await new Promise((resolve) => server.close(resolve));
    await unlink(path).catch(() => {});
    throw error;
  }
  return async () => {
    for (const client of clients) client.destroy();
    await new Promise((resolve) => server.close(resolve));
    await unlink(path).catch((error) => { if (error.code !== "ENOENT") throw error; });
  };
}

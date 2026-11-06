import { mkdtemp, mkdir, rm } from "node:fs/promises";
import { createServer, type Socket } from "node:net";
import { dirname, join } from "node:path";

export async function isolatedState() {
  // Short paths also fit Darwin's 104-byte sockaddr_un limit.
  const root = await mkdtemp("/tmp/cito-adapters-");
  const stateDir = join(root, "state");
  const codexHome = join(root, "codex"), claudeHome = join(root, "claude");
  await Promise.all([mkdir(stateDir), mkdir(codexHome), mkdir(claudeHome)]);
  return { root, stateDir, codexHome, claudeHome,
    env: { ...process.env, CITO_STATE_DIR: stateDir, CITO_CODEX_HOME: codexHome, CITO_CLAUDE_HOME: claudeHome },
    cleanup: () => rm(root, { recursive: true, force: true }) };
}
export async function unixServer(path: string, handler: (socket: Socket) => void) {
  await mkdir(dirname(path), { recursive: true });
  const sockets = new Set<Socket>();
  const server = createServer(socket => {
    sockets.add(socket);
    socket.on("error", () => socket.destroy());
    socket.on("close", () => sockets.delete(socket));
    handler(socket);
  });
  await new Promise<void>((resolve, reject) => { server.once("error", reject); server.listen(path, resolve); });
  return { stop: async () => {
    for (const socket of sockets) socket.destroy();
    await new Promise<void>((resolve, reject) => server.close(error => error ? reject(error) : resolve()));
  } };
}

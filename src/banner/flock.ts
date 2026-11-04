import { spawn } from "node:child_process";

// macOS ships Perl and these core modules. The child holds a real flock(2)
// descriptor until stdin closes, interoperating with Darwin.flock on the exact
// same inode. No lockfile deletion, PID guessing, or advisory JS-only mutex.
const program = String.raw`
use strict; use warnings; use Fcntl qw(:DEFAULT :flock); use Time::HiRes qw(time sleep);
sysopen(my $lock, $ARGV[0], O_CREAT | O_RDWR, 0600) or die "open lock: $!";
my $deadline = time() + $ARGV[1] / 1000;
while (!flock($lock, LOCK_EX | LOCK_NB)) {
  if (!$!{EWOULDBLOCK} && !$!{EAGAIN} && !$!{EINTR}) { die "flock: $!"; }
  if (time() >= $deadline) { exit 75; }
  sleep(0.01);
}
$| = 1; print "locked\n";
my $byte; while (sysread(STDIN, $byte, 1)) {}
`;

export interface PosixLock {
  /** Resolves only after the kernel has released the lock. Idempotent. */
  release(): Promise<void>;
}

/** null means contention/timeout; all other failures reject. */
export async function acquireFlock(path: string, timeoutMs: number): Promise<PosixLock | null> {
  const child = spawn("/usr/bin/perl", ["-e", program, path, String(Math.max(0, timeoutMs))], {
    stdio: ["pipe", "pipe", "pipe"],
  });
  let stderr = "";
  child.stderr.setEncoding("utf8").on("data", (chunk: string) => { stderr += chunk; });
  child.stdin.on("error", () => { /* exit handler reports acquisition errors */ });
  const exited = new Promise<number | null>((resolve) => child.once("exit", resolve));
  return new Promise((resolve, reject) => {
    let settled = false;
    const watchdog = setTimeout(() => {
      if (settled) return;
      child.kill("SIGKILL");
      settled = true;
      reject(new Error("flock helper did not respond"));
    }, Math.max(0, timeoutMs) + 1000);
    child.once("error", (error) => {
      clearTimeout(watchdog);
      if (!settled) { settled = true; reject(error); }
    });
    let output = "";
    child.stdout.setEncoding("utf8").on("data", (chunk: string) => {
      output += chunk;
      if (settled || !output.includes("locked\n")) return;
      settled = true;
      clearTimeout(watchdog);
      let released: Promise<void> | undefined;
      resolve({ release() {
        return released ??= (async () => { child.stdin.end(); await exited; })();
      } });
    });
    child.once("close", (code) => {
      clearTimeout(watchdog);
      if (settled) return;
      settled = true;
      if (code === 75) resolve(null);
      else reject(new Error(`flock helper exited ${code}: ${stderr.trim()}`));
    });
  });
}

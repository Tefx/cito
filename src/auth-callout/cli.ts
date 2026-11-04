import { loadAuthCalloutConfig } from "./config.js";
import { startAuthCallout } from "./responder.js";

export async function main(): Promise<void> {
  const config = loadAuthCalloutConfig();
  try {
    const service = await startAuthCallout(config, {
      onError: error => process.stderr.write("cito-auth-callout: " + error.message + "\n"),
    });
    let stopping: Promise<void> | undefined;
    const stop = () => { void (stopping ??= service.stop()).catch(error => {
      process.stderr.write("cito-auth-callout: shutdown failed: " + String(error) + "\n");
      process.exitCode = 1;
    }); };
    process.once("SIGINT", stop);
    process.once("SIGTERM", stop);
    process.stderr.write("cito-auth-callout: ready\n");
    try {
      const error = await service.connection.closed();
      if (error) throw error;
      await stopping;
    } finally {
      await service.stop();
      process.removeListener("SIGINT", stop);
      process.removeListener("SIGTERM", stop);
    }
  } finally { config.issuer.clear(); config.xkey?.clear(); }
}

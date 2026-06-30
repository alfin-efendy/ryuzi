export function composerMode(status: string | undefined): "send" | "stop" {
  return status === "running" ? "stop" : "send";
}

import { symbolsUnicode, symbolsAscii } from "./tokens";

export function unicodeEnabled(): boolean {
  return !process.env.HR_ASCII;
}
export function symbols() {
  return unicodeEnabled() ? symbolsUnicode : symbolsAscii;
}
export function borderStyle(): "round" | "single" {
  return unicodeEnabled() ? "round" : "single";
}
export function colorEnabled(): boolean {
  return Boolean(process.stdout.isTTY) && !process.env.NO_COLOR && process.env.TERM !== "dumb";
}

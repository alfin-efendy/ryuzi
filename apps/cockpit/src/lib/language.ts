import { LanguageDescription } from "@codemirror/language";
import { languages } from "@codemirror/language-data";

/** Match a CodeMirror language pack by filename; null = render plain text. */
export function languageFor(filename: string): LanguageDescription | null {
  return LanguageDescription.matchFilename(languages, filename) ?? null;
}

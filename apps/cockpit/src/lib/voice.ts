type SpeechRecognitionCtor = new () => {
  continuous: boolean;
  interimResults: boolean;
  lang: string;
  onresult: ((event: unknown) => void) | null;
  onerror: ((event: unknown) => void) | null;
  onend: (() => void) | null;
  start: () => void;
  stop: () => void;
};

type VoiceCallbacks = {
  onText: (text: string) => void;
  onEnd: () => void;
  onError: (message: string) => void;
};

type VoiceStartResult = { ok: true; stop: () => void } | { ok: false; message: string };

export function startVoiceDictation(callbacks: VoiceCallbacks): VoiceStartResult {
  const w = window as typeof window & {
    SpeechRecognition?: SpeechRecognitionCtor;
    webkitSpeechRecognition?: SpeechRecognitionCtor;
  };
  const Recognition = w.SpeechRecognition ?? w.webkitSpeechRecognition;
  if (!Recognition) {
    return { ok: false, message: "Voice input is not available in this WebView." };
  }

  const recognition = new Recognition();
  recognition.continuous = false;
  recognition.interimResults = false;
  recognition.lang = navigator.language || "en-US";
  recognition.onresult = (event: unknown) => {
    const results = (event as { results?: ArrayLike<ArrayLike<{ transcript?: string }>> }).results;
    if (!results) return;
    let text = "";
    for (let i = 0; i < results.length; i += 1) {
      text += results[i]?.[0]?.transcript ?? "";
    }
    const trimmed = text.trim();
    if (trimmed) callbacks.onText(trimmed);
  };
  recognition.onerror = (event: unknown) => {
    const error = (event as { error?: string }).error;
    callbacks.onError(error ? `Voice input failed: ${error}` : "Voice input failed.");
  };
  recognition.onend = callbacks.onEnd;
  recognition.start();
  return { ok: true, stop: () => recognition.stop() };
}

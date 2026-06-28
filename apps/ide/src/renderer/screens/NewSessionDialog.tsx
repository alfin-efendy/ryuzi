import React, { useState } from "react";
import { useStore } from "../store";
import { Dialog, DialogContent, DialogHeader, DialogTitle, DialogTrigger } from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";

export function NewSessionDialog({ projectId }: { projectId: string }) {
  const [open, setOpen] = useState(false);
  const [prompt, setPrompt] = useState("");
  const [error, setError] = useState<string | null>(null);
  const connId = useStore((s) => s.connId);
  const setSessions = useStore((s) => s.setSessions);
  const setActive = useStore((s) => s.setActive);

  async function start() {
    setError(null);
    const text = prompt.trim();
    if (!text || !connId) return;
    try {
      const session = await window.harness.startSession({ projectId, prompt: text, surface: { gateway: "ide", conversationId: connId } });
      setSessions(await window.harness.listSessions());
      setActive(session.sessionPk);
      setPrompt("");
      setOpen(false);
    } catch (e) {
      setError((e as Error).message);
    }
  }

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger asChild>
        <Button size="sm" variant="ghost" className="h-6 px-2 text-xs">
          + New session
        </Button>
      </DialogTrigger>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>New session</DialogTitle>
        </DialogHeader>
        <div className="space-y-3">
          <Input placeholder="prompt…" value={prompt} onChange={(e) => setPrompt(e.target.value)} />
          {error && <p className="text-xs text-destructive">{error}</p>}
          <Button onClick={() => void start()}>Start</Button>
        </div>
      </DialogContent>
    </Dialog>
  );
}

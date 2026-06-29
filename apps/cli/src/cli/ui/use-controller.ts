import { useEffect, useReducer } from "react";
import type { AppController } from "./controller";

export function useController(controller: AppController): void {
  const [, force] = useReducer((x: number) => x + 1, 0);
  useEffect(() => {
    const cb = () => force();
    controller.on("change", cb);
    return () => {
      controller.off("change", cb);
    };
  }, [controller]);
}

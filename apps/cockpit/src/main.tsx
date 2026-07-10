import React from "react";
import { createRoot } from "react-dom/client";
import { initTheme } from "@ryuzi/ui";
import App from "./App";
import { initShell } from "./lib/shell-init";
import { initNotifications } from "@/lib/notify";
import "./index.css";

initTheme();
createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
void initShell();
initNotifications();

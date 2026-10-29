// Dev-only: install the mock Tauri IPC BEFORE anything imports @tauri-apps/api
// (this side-effect import must stay first). It self-guards to dev + plain
// browser and is dead-code-eliminated from production builds.
import "./devMock";

import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import "./index.css";
import { installLinkGuard } from "./safeContent";
import { CloudConsentOnLaunch } from "./components/CloudConsent";
import { ConfirmProvider } from "./components/confirm";
import LibraryGate from "./components/LibraryGate";

// Links anywhere in the app open in the system browser, never in this window.
installLinkGuard();

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    {/* Nothing that talks to the library mounts before it's open: a first
        launch after an update may be upgrading it in the background. */}
    <LibraryGate>
      {/* Every confirm in the app is an in-app dialog (useConfirm). */}
      <ConfirmProvider>
        <App />
        <CloudConsentOnLaunch />
      </ConfirmProvider>
    </LibraryGate>
  </React.StrictMode>,
);

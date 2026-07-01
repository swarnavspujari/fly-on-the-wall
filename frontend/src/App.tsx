import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

interface AppInfo {
  version: string;
  data_dir: string;
}

export default function App() {
  const [pong, setPong] = useState<string | null>(null);
  const [info, setInfo] = useState<AppInfo | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    invoke<string>("ping")
      .then(setPong)
      .catch((e) => setError(String(e)));
    invoke<AppInfo>("app_info")
      .then(setInfo)
      .catch((e) => setError(String(e)));
  }, []);

  return (
    <div className="flex h-screen flex-col items-center justify-center bg-zinc-950 text-zinc-100">
      <div className="flex flex-col items-center gap-4">
        <h1 className="text-5xl font-semibold tracking-tight">Looma</h1>
        <p className="text-zinc-400">Local-first meeting notes. Your machine, your models.</p>
        <div className="mt-6 rounded-lg border border-zinc-800 bg-zinc-900 px-6 py-4 text-sm">
          <div className="flex items-center gap-2">
            <span
              className={`inline-block h-2 w-2 rounded-full ${
                pong === "pong" ? "bg-emerald-500" : "bg-amber-500"
              }`}
            />
            <span>
              Backend: {pong === "pong" ? "connected" : error ? `error — ${error}` : "connecting…"}
            </span>
          </div>
          {info && (
            <div className="mt-2 text-zinc-500">
              v{info.version} · data at {info.data_dir}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

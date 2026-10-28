import js from "@eslint/js";
import tseslint from "typescript-eslint";
import reactHooks from "eslint-plugin-react-hooks";

// In the app, tauri-plugin-dialog swaps confirm/alert for async versions
// (a Promise is always truthy, so `if (confirm(...))` never waits) whose IPC
// isn't granted, so no dialog shows. Ask in-app instead.
const NO_NATIVE_DIALOG =
  "Native dialogs don't work in the app (tauri-plugin-dialog makes them async). " +
  "Use `await confirm({...})` from useConfirm() in components/confirm.tsx.";

export default tseslint.config(
  { ignores: ["dist"] },
  js.configs.recommended,
  ...tseslint.configs.recommended,
  {
    files: ["src/**/*.{ts,tsx}"],
    plugins: { "react-hooks": reactHooks },
    rules: {
      ...reactHooks.configs.recommended.rules,
      // Our effects fetch from the Tauri backend and set state when data
      // arrives — the standard desktop-app pattern this rule over-flags.
      "react-hooks/set-state-in-effect": "off",
      "no-restricted-globals": [
        "error",
        { name: "confirm", message: NO_NATIVE_DIALOG },
        { name: "alert", message: NO_NATIVE_DIALOG },
      ],
      "no-restricted-properties": [
        "error",
        ...["window", "globalThis", "self"].flatMap((object) =>
          ["confirm", "alert"].map((property) => ({
            object,
            property,
            message: NO_NATIVE_DIALOG,
          })),
        ),
      ],
    },
  },
);

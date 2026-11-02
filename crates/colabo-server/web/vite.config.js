import { defineConfig } from "vite";
import { fileURLToPath } from "node:url";

export default defineConfig({
  base: "/",
  build: {
    outDir: "../static/build",
    emptyOutDir: true,
    assetsDir: "assets",
    rollupOptions: {
      input: {
        index: fileURLToPath(new URL("./index.html", import.meta.url)),
        "collab-editor": fileURLToPath(new URL("./src/embed.js", import.meta.url)),
      },
      output: {
        entryFileNames: "assets/[name].js",
        chunkFileNames: "assets/chunks/[name]-[hash].js",
        assetFileNames: (asset) => {
          if (asset.name?.endsWith(".css")) {
            const source = typeof asset.source === "string"
              ? asset.source
              : asset.source
                ? Buffer.from(asset.source).toString("utf8")
                : "";
            if (source.includes(".collab-gutter-presence-layer")) return "assets/collab-editor.css";
            if (source.includes(".monaco-editor .")) {
              return source.includes(".page")
                ? "assets/collab-editor-runtime.css"
                : "assets/monaco-editor.css";
            }
          }
          return "assets/[name]-[hash][extname]";
        },
      },
    },
  },
});

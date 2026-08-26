import path from "node:path";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { defineConfig } from "vite";
import Icons from "unplugin-icons/vite";

export default defineConfig({
  root: "src",
  base: "./",
  plugins: [react(), tailwindcss(), Icons({ compiler: "jsx", jsx: "react" })],
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
      "@edger/ui": path.resolve(__dirname, "../../ui/src"),
    },
  },
  publicDir: false,
  build: {
    target: "es2022",
    outDir: "../dist",
    emptyOutDir: true,
    rollupOptions: {
      output: {
        // Fingerprinted under assets/: the runtime pins these as immutable
        // while the HTML shell stays no-cache — a stale shell once kept the
        // old SPA (and its bugs) alive across deploys.
        entryFileNames: "assets/[name]-[hash].js",
        chunkFileNames: "assets/[name]-[hash].js",
        assetFileNames: "assets/[name]-[hash][extname]",
      },
    },
  },
});

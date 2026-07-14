import { defineConfig } from "vite";
import Icons from "unplugin-icons/vite";

export default defineConfig({
  root: "src",
  base: "./",
  plugins: [Icons()],
  publicDir: false,
  build: {
    target: "es2022",
    outDir: "../dist",
    emptyOutDir: true,
    rollupOptions: {
      output: {
        entryFileNames: "app.js",
        chunkFileNames: "[name].js",
        assetFileNames: (asset) => asset.names?.some((name) => name.endsWith(".css")) ? "styles.css" : "[name][extname]",
      },
    },
  },
});

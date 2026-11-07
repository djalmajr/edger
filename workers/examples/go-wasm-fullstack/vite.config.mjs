// Plain object, no imports: it builds with any installed Vite and needs no
// extra dependency. `base: './'` keeps every asset URL relative, so the SPA
// works under the name route, a pinned version route or a vhost; the EdgeR
// injects the real <base href> into index.html.
export default {
  base: './',
  build: {
    outDir: 'dist',
    emptyOutDir: true,
  },
};

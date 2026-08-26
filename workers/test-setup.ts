// bun test não tem DOM; o registrator do happy-dom instala window/document
// globais antes dos arquivos de teste (preload via bunfig.toml). Os testes
// puros não notam; os de componente (badge) passam a ter onde renderizar.
import { GlobalRegistrator } from "@happy-dom/global-registrator";

// URL real: o cpanel deriva o runtime root do document.baseURI, e o
// "about:blank" default do happy-dom estoura o new URL do módulo.
GlobalRegistrator.register({ url: "http://localhost/cpanel/" });

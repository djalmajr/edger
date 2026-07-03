const manifest = (() => {
function __memo(fn) {
	let value;
	return () => value ??= (value = fn());
}

return {
	appDir: "_app",
	appPath: "sveltekit-demo/_app",
	assets: new Set(["robots.txt"]),
	mimeTypes: {".txt":"text/plain"},
	_: {
		client: {start:"_app/immutable/entry/start.DuAYF7Wd.js",app:"_app/immutable/entry/app.DTy49Z1H.js",imports:["_app/immutable/entry/start.DuAYF7Wd.js","_app/immutable/chunks/Bo-2O-Mw.js","_app/immutable/chunks/Ce41ZjGq.js","_app/immutable/entry/app.DTy49Z1H.js","_app/immutable/chunks/Ce41ZjGq.js","_app/immutable/chunks/DYl5dUZ5.js","_app/immutable/chunks/xihTtKlq.js"],stylesheets:[],fonts:[],uses_env_dynamic_public:false},
		nodes: [
			__memo(() => import('./nodes/0.js-Dj4jMpqN.js')),
			__memo(() => import('./nodes/1.js-DN5P8VFh.js')),
			__memo(() => import('./nodes/2.js-DJYxOz2p.js'))
		],
		remotes: {
			
		},
		routes: [
			{
				id: "/",
				pattern: /^\/$/,
				params: [],
				page: { layouts: [0,], errors: [1,], leaf: 2 },
				endpoint: null
			},
			{
				id: "/api/info",
				pattern: /^\/api\/info\/?$/,
				params: [],
				page: null,
				endpoint: __memo(() => import('./entries/endpoints/api/info/_server.ts.js-Bc2HGA72.js'))
			}
		],
		prerendered_routes: new Set([]),
		matchers: async () => {
			
			return {  };
		},
		server_assets: {}
	}
}
})();

export { manifest as m };
//# sourceMappingURL=manifest.js-DZz-Diuw.js.map

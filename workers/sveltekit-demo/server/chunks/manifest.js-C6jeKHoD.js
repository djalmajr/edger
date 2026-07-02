const manifest = (() => {
function __memo(fn) {
	let value;
	return () => value ??= (value = fn());
}

return {
	appDir: "_app",
	appPath: "_app",
	assets: new Set(["robots.txt"]),
	mimeTypes: {".txt":"text/plain"},
	_: {
		client: {start:"_app/immutable/entry/start.C9zjJMBO.js",app:"_app/immutable/entry/app.CPGyd6Er.js",imports:["_app/immutable/entry/start.C9zjJMBO.js","_app/immutable/chunks/svYQDs1R.js","_app/immutable/chunks/Ce41ZjGq.js","_app/immutable/entry/app.CPGyd6Er.js","_app/immutable/chunks/Ce41ZjGq.js","_app/immutable/chunks/DYl5dUZ5.js","_app/immutable/chunks/xihTtKlq.js"],stylesheets:[],fonts:[],uses_env_dynamic_public:false},
		nodes: [
			__memo(() => import('./nodes/0.js-Dj4jMpqN.js')),
			__memo(() => import('./nodes/1.js-DbXxgjza.js')),
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
				endpoint: __memo(() => import('./entries/endpoints/api/info/_server.ts.js-5SzQgq-C.js'))
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
//# sourceMappingURL=manifest.js-C6jeKHoD.js.map

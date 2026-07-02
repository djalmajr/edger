//#region node_modules/@sveltejs/kit/src/runtime/app/paths/internal/server.js
var base = "/sveltekit-demo";
var assets = base;
var app_dir = "_app";
var initial = {
	base,
	assets
};
function reset() {
	base = initial.base;
	assets = initial.assets;
}
/**
* `$env/dynamic/public`
* @type {Record<string, string>}
*/
var public_env = {};
/** @type {(environment: Record<string, string>) => void} */
function set_private_env(environment) {}
/** @type {(environment: Record<string, string>) => void} */
function set_public_env(environment) {
	public_env = environment;
}

export { set_public_env as a, base as b, assets as c, app_dir as d, public_env as p, reset as r, set_private_env as s };
//# sourceMappingURL=internal2.js-DTokQbBy.js.map

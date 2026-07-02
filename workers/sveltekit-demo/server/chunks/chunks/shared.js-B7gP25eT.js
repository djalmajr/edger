import { T as parse, U as stringify$1 } from './server.js-DbjiTjSX.js';

/** @import { StandardSchemaV1 } from '@standard-schema/spec' */

class HttpError {
	/**
	 * @param {number} status
	 * @param {{message: string} extends App.Error ? (App.Error | string | undefined) : App.Error} body
	 */
	constructor(status, body) {
		this.status = status;
		if (typeof body === 'string') {
			this.body = { message: body };
		} else if (body) {
			this.body = body;
		} else {
			this.body = { message: `Error: ${status}` };
		}
	}

	toString() {
		return JSON.stringify(this.body);
	}
}

class Redirect {
	/**
	 * @param {300 | 301 | 302 | 303 | 304 | 305 | 306 | 307 | 308} status
	 * @param {string} location
	 */
	constructor(status, location) {
		try {
			new Headers({ location });
		} catch {
			throw new Error(
				`Invalid redirect location ${JSON.stringify(location)}: ` +
					'this string contains characters that cannot be used in HTTP headers'
			);
		}

		this.status = status;
		this.location = location;
	}
}

/**
 * An error that was thrown from within the SvelteKit runtime that is not fatal and doesn't result in a 500, such as a 404.
 * `SvelteKitError` goes through `handleError`.
 * @extends Error
 */
class SvelteKitError extends Error {
	/**
	 * @param {number} status
	 * @param {string} text
	 * @param {string} message
	 */
	constructor(status, text, message) {
		super(message);
		this.status = status;
		this.text = text;
	}
}

/**
 * @template [T=undefined]
 */
class ActionFailure {
	/**
	 * @param {number} status
	 * @param {T} data
	 */
	constructor(status, data) {
		this.status = status;
		this.data = data;
	}
}

//#region node_modules/@sveltejs/kit/src/utils/functions.js
function noop() {}
/**
* @template T
* @param {() => T} fn
*/
function once(fn) {
	let done = false;
	/** @type T */
	let result;
	return () => {
		if (done) return result;
		done = true;
		return result = fn();
	};
}
//#endregion
//#region node_modules/@sveltejs/kit/src/runtime/utils.js
var text_encoder = new TextEncoder();
/**
* @param {Uint8Array} bytes
* @returns {string}
*/
function base64_encode(bytes) {
	if (globalThis.Buffer) return globalThis.Buffer.from(bytes).toString("base64");
	let binary = "";
	for (let i = 0; i < bytes.length; i++) binary += String.fromCharCode(bytes[i]);
	return btoa(binary);
}
/**
* @param {string} encoded
* @returns {Uint8Array}
*/
function base64_decode(encoded) {
	if (globalThis.Buffer) {
		const buffer = globalThis.Buffer.from(encoded, "base64");
		return new Uint8Array(buffer);
	}
	const binary = atob(encoded);
	const bytes = new Uint8Array(binary.length);
	for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
	return bytes;
}
//#endregion
//#region node_modules/@sveltejs/kit/src/utils/error.js
/**
* @param {unknown} err
* @return {Error}
*/
function coalesce_to_error(err) {
	return err instanceof Error || err && err.name && err.message ? err : new Error(JSON.stringify(err));
}
/**
* This is an identity function that exists to make TypeScript less
* paranoid about people throwing things that aren't errors, which
* frankly is not something we should care about
* @param {unknown} error
*/
function normalize_error(error) {
	return error;
}
/**
* @param {unknown} error
*/
function get_status(error) {
	return error instanceof HttpError || error instanceof SvelteKitError ? error.status : 500;
}
/**
* @param {unknown} error
*/
function get_message(error) {
	return error instanceof SvelteKitError ? error.text : "Internal Error";
}
var INVALIDATED_PARAM = "x-sveltekit-invalidated";
var TRAILING_SLASH_PARAM = "x-sveltekit-trailing-slash";
/**
* Try to `devalue.stringify` the data object using the provided transport encoders.
* @param {any} data
* @param {Transport} transport
*/
function stringify(data, transport) {
	const encoders = Object.fromEntries(Object.entries(transport).map(([k, v]) => [k, v.encode]));
	return stringify$1(data, encoders);
}
var remote_object = "__skrao";
var remote_map = "__skram";
var remote_set = "__skras";
var remote_file = "__skraf";
/** @param {Transport} transport */
function create_remote_arg_revivers(transport) {
	const remote_fns_revivers = {
		/** @type {(value: unknown) => unknown} */
		[remote_object]: (value) => value,
		/** @type {(value: unknown) => Map<unknown, unknown>} */
		[remote_map]: (value) => {
			if (!Array.isArray(value)) throw new Error("Invalid data for Map reviver");
			const map = /* @__PURE__ */ new Map();
			for (const item of value) {
				if (!Array.isArray(item) || item.length !== 2 || typeof item[0] !== "string" || typeof item[1] !== "string") throw new Error("Invalid data for Map reviver");
				const [key, val] = item;
				map.set(parse$1(key), parse$1(val));
			}
			return map;
		},
		/** @type {(value: unknown) => Set<unknown>} */
		[remote_set]: (value) => {
			if (!Array.isArray(value)) throw new Error("Invalid data for Set reviver");
			const set = /* @__PURE__ */ new Set();
			for (const item of value) {
				if (typeof item !== "string") throw new Error("Invalid data for Set reviver");
				set.add(parse$1(item));
			}
			return set;
		},
		/** @type {(value: any) => File} */
		[remote_file]: (value) => {
			if (!value || typeof value !== "object" || typeof value.name !== "string" || typeof value.type !== "string" || typeof value.size !== "number" || typeof value.lastModified !== "number" || !(value.data instanceof ArrayBuffer)) throw new Error("Invalid data for File reviver");
			const { data, name, ...meta } = value;
			return new File([data], name, meta);
		}
	};
	const all_revivers = {
		...Object.fromEntries(Object.entries(transport).map(([k, v]) => [k, v.decode])),
		...remote_fns_revivers
	};
	/** @type {(data: string) => unknown} */
	const parse$1 = (data) => parse(data, all_revivers);
	return all_revivers;
}
/**
* Parses the argument (if any) for a remote function
* @param {string} string
* @param {Transport} transport
*/
function parse_remote_arg(string, transport) {
	if (!string) return void 0;
	const json_string = new TextDecoder().decode(base64_decode(string.replaceAll("-", "+").replaceAll("_", "/")));
	return parse(json_string, create_remote_arg_revivers(transport));
}
/**
* @param {string} id
* @param {string} payload
*/
function create_remote_key(id, payload) {
	return id + "/" + payload;
}
/**
* @param {string} key
* @returns {{ id: string; payload: string }}
*/
function split_remote_key(key) {
	const i = key.lastIndexOf("/");
	if (i === -1) throw new Error(`Invalid remote key: ${key}`);
	return {
		id: key.slice(0, i),
		payload: key.slice(i + 1)
	};
}

export { ActionFailure as A, HttpError as H, INVALIDATED_PARAM as I, Redirect as R, SvelteKitError as S, TRAILING_SLASH_PARAM as T, get_message as a, normalize_error as b, coalesce_to_error as c, create_remote_key as d, split_remote_key as e, base64_encode as f, get_status as g, noop as n, once as o, parse_remote_arg as p, stringify as s, text_encoder as t };
//# sourceMappingURL=shared.js-B7gP25eT.js.map

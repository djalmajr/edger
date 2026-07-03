import { Z as escape_html } from '../../chunks/server.js-DbjiTjSX.js';

//#region src/routes/+page.svelte
function _page($$renderer, $$props) {
	$$renderer.component(($$renderer) => {
		let { data } = $$props;
		$$renderer.push(`<h1>SvelteKit no EdgeR</h1> <p data-testid="ssr">rendered-on-server:${escape_html(data.answer)}</p> <p>at ${escape_html(data.renderedAt)}</p>`);
	});
}

export { _page as default };
//# sourceMappingURL=_page.svelte.js--ZksX9bK.js.map

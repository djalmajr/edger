import { j as json } from '../../../../chunks/utils.js-CSEycWRK.js';
import '../../../../chunks/shared.js-B7gP25eT.js';
import '../../../../chunks/server.js-DbjiTjSX.js';

//#region src/routes/api/info/+server.ts
function GET() {
	return json({
		framework: "sveltekit",
		runtime: "edger"
	});
}

export { GET };
//# sourceMappingURL=_server.ts.js-Bc2HGA72.js.map

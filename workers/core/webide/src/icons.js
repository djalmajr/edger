import browser from "~icons/lucide/panels-top-left?raw";
import check from "~icons/lucide/check?raw";
import chevron from "~icons/lucide/chevron-right?raw";
import close from "~icons/lucide/x?raw";
import code from "~icons/lucide/braces?raw";
import copy from "~icons/lucide/copy?raw";
import deploy from "~icons/lucide/upload?raw";
import edit from "~icons/lucide/pencil?raw";
import external from "~icons/lucide/external-link?raw";
import eye from "~icons/lucide/eye?raw";
import file from "~icons/lucide/files?raw";
import filePlus from "~icons/lucide/file-plus-2?raw";
import folder from "~icons/lucide/folder?raw";
import folderPlus from "~icons/lucide/folder-plus?raw";
import grid from "~icons/lucide/layout-dashboard?raw";
import importProject from "~icons/lucide/folder-input?raw";
import logs from "~icons/lucide/list?raw";
import logo from "~icons/lucide/asterisk?raw";
import more from "~icons/lucide/ellipsis?raw";
import nextjs from "~icons/lucide/circle-dot?raw";
import plus from "~icons/lucide/plus?raw";
import react from "~icons/lucide/atom?raw";
import refresh from "~icons/lucide/refresh-cw?raw";
import route from "~icons/lucide/route?raw";
import search from "~icons/lucide/search?raw";
import stack from "~icons/lucide/layers-3?raw";
import svelte from "~icons/lucide/flame?raw";
import tanstack from "~icons/lucide/table-2?raw";
import terminal from "~icons/lucide/square-terminal?raw";
import trash from "~icons/lucide/trash-2?raw";
import vue from "~icons/lucide/boxes?raw";

const icons = {
  browser,
  check,
  chevron,
  close,
  code,
  copy,
  deploy,
  edit,
  external,
  eye,
  file,
  filePlus,
  folder,
  folderPlus,
  grid,
  import: importProject,
  logs,
  logo,
  more,
  nextjs,
  plus,
  react,
  refresh,
  route,
  search,
  stack,
  svelte,
  tanstack,
  terminal,
  trash,
  vue,
};

export function icon(name, size = 16) {
  const source = icons[name] || icons.file;
  return source.replace(/<svg\b([^>]*)>/, (_match, attributes) => {
    const normalized = attributes
      .replace(/\s(?:width|height)="[^"]*"/g, "")
      .replace(/\saria-hidden="[^"]*"/g, "");
    return `<svg aria-hidden="true" data-icon="inline-start" width="${size}" height="${size}"${normalized}>`;
  });
}

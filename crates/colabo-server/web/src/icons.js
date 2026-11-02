import arrowRight from "./tabler-icons/arrow-right.svg?raw";
import check from "./tabler-icons/check.svg?raw";
import chevronDown from "./tabler-icons/chevron-down.svg?raw";
import dots from "./tabler-icons/dots.svg?raw";
import download from "./tabler-icons/download.svg?raw";
import eye from "./tabler-icons/eye.svg?raw";
import externalLink from "./tabler-icons/external-link.svg?raw";
import file from "./tabler-icons/file.svg?raw";
import fileCode from "./tabler-icons/file-code.svg?raw";
import filePlus from "./tabler-icons/file-plus.svg?raw";
import fileText from "./tabler-icons/file-text.svg?raw";
import folder from "./tabler-icons/folder.svg?raw";
import folderOpen from "./tabler-icons/folder-open.svg?raw";
import folderPlus from "./tabler-icons/folder-plus.svg?raw";
import history from "./tabler-icons/history.svg?raw";
import layoutColumns from "./tabler-icons/layout-columns.svg?raw";
import layoutRows from "./tabler-icons/layout-rows.svg?raw";
import logout from "./tabler-icons/logout.svg?raw";
import messages from "./tabler-icons/messages.svg?raw";
import minus from "./tabler-icons/minus.svg?raw";
import pencil from "./tabler-icons/pencil.svg?raw";
import plus from "./tabler-icons/plus.svg?raw";
import refresh from "./tabler-icons/refresh.svg?raw";
import search from "./tabler-icons/search.svg?raw";
import settings from "./tabler-icons/settings.svg?raw";
import trash from "./tabler-icons/trash.svg?raw";
import upload from "./tabler-icons/upload.svg?raw";
import userCircle from "./tabler-icons/user-circle.svg?raw";
import users from "./tabler-icons/users.svg?raw";
import x from "./tabler-icons/x.svg?raw";

const ICONS = {
  "arrow-right": arrowRight,
  check,
  "chevron-down": chevronDown,
  dots,
  download,
  eye,
  "external-link": externalLink,
  file,
  "file-code": fileCode,
  "file-plus": filePlus,
  "file-text": fileText,
  folder,
  "folder-open": folderOpen,
  "folder-plus": folderPlus,
  history,
  "layout-columns": layoutColumns,
  "layout-rows": layoutRows,
  logout,
  messages,
  minus,
  pencil,
  plus,
  refresh,
  search,
  settings,
  trash,
  upload,
  "user-circle": userCircle,
  users,
  x,
};

export function tablerIcon(name) {
  const svg = ICONS[name];
  return svg?.replace(/<svg\b/, '<svg class="tabler-icon" aria-hidden="true" focusable="false"') || "";
}

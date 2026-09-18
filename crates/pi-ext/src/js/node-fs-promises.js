import { promises } from "node:fs";
export const { readFile, writeFile, appendFile, mkdir, readdir, stat, lstat, unlink, rm, rename, copyFile, access, realpath, mkdtemp } = promises;
export default promises;

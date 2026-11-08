/** MariaDB 11.8 mock: shop (with vectors) and analytics databases; vector operations through `invoke_capability`. */
import { DESCRIPTORS, ORDER } from "../descriptors";
import { mysqlFamily } from "../mysqlFamily";

const engine = mysqlFamily(DESCRIPTORS.mariadb, ORDER.indexOf("mariadb"), "MariaDB", "11.8.2-MariaDB", "MariaDB 11.8.2");

export const { descriptor, order, connectInfo, tree, describe, browse, apply, run, classify, invoke, schemaGraph, completion, createDatabase, touchRow } =
  engine;

/** MySQL 8.4 mock: its own copy of the shop and analytics databases (no vector operations). */
import { DESCRIPTORS, ORDER } from "../descriptors";
import { mysqlFamily } from "../mysqlFamily";

const engine = mysqlFamily(DESCRIPTORS.mysql, ORDER.indexOf("mysql"), "MySQL", "8.4.3", "MySQL 8.4.3");

export const { descriptor, order, connectInfo, tree, describe, browse, apply, run, classify, invoke, schemaGraph, completion, createDatabase, touchRow } =
  engine;

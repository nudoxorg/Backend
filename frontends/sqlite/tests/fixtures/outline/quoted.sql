CREATE TABLE "Mixed Case" ("Id" INTEGER PRIMARY KEY, `back``tick` TEXT, [br acket] TEXT, "dq""x" TEXT);
SELECT "Mixed Case"."Id", mc.`back``tick` AS x FROM "Mixed Case" AS mc WHERE "dq""x" = 1 /* c1 */ AND /* c2 */ [br acket] /* c3 */ . /* c4 */ y = 2;
SELECT "not_a_column" FROM t;
SELECT t -- trailing
  . /* mid */ col FROM t;

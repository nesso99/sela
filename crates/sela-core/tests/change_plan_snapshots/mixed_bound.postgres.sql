DELETE FROM "public"."items" WHERE "tenant_id" = $1 AND "id" IS NULL AND "name" = $2 AND "qty" = $3 AND "note" IS NULL
UPDATE "public"."items" SET "name" = $1, "qty" = $2 WHERE "tenant_id" = $3 AND "id" = $4 AND "name" = $5 AND "qty" = $6
INSERT INTO "public"."items" ("tenant_id", "id", "name", "qty", "note") VALUES ($1, $2, $3, $4, $5)

DELETE FROM "public"."items" WHERE "tenant_id" = ? AND "id" IS NULL
UPDATE "public"."items" SET "name" = ?, "qty" = ? WHERE "tenant_id" = ? AND "id" = ?
INSERT INTO "public"."items" ("tenant_id", "id", "name", "qty", "note") VALUES (?, ?, ?, ?, ?)

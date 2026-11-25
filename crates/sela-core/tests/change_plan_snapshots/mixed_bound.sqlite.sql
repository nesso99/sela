DELETE FROM "public"."items" WHERE "tenant_id" = ? AND "id" IS NULL AND "name" = ? AND "qty" = ? AND "note" IS NULL
UPDATE "public"."items" SET "name" = ?, "qty" = ? WHERE "tenant_id" = ? AND "id" = ? AND "name" = ? AND "qty" = ?
INSERT INTO "public"."items" ("tenant_id", "id", "name", "qty", "note") VALUES (?, ?, ?, ?, ?)

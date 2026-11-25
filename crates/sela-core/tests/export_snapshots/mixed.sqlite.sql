INSERT INTO "public"."books" ("id", "title", "note", "blob", "flag") VALUES
(1, 'a,b', NULL, X'DEADBEEF', true),
(2, 'say "hi"
there', '', X'', false);
INSERT INTO "public"."books" ("id", "title", "note", "blob", "flag") VALUES
(3, 'café', 'NULL', X'00', NULL),
(4, 'cronly', 'plain', X'FF', true);

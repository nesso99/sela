INSERT INTO "public"."books" ("id", "title", "note", "blob", "flag") VALUES
(1, 'a,b', NULL, '\xDEADBEEF', true),
(2, 'say "hi"
there', '', '\x', false);
INSERT INTO "public"."books" ("id", "title", "note", "blob", "flag") VALUES
(3, 'café', 'NULL', '\x00', NULL),
(4, 'cronly', 'plain', '\xFF', true);

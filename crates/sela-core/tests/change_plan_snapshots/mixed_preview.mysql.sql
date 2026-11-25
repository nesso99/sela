BEGIN;
DELETE FROM `public`.`items` WHERE `tenant_id` = 2 AND `id` IS NULL AND `name` = 'beta' AND `qty` = 0 AND `note` IS NULL;
UPDATE `public`.`items` SET `name` = NULL, `qty` = 7 WHERE `tenant_id` = 1 AND `id` = 10 AND `name` = 'alpha' AND `qty` = 3;
INSERT INTO `public`.`items` (`tenant_id`, `id`, `name`, `qty`, `note`) VALUES (1, 99, 'O''Reilly', 1, NULL);
COMMIT;

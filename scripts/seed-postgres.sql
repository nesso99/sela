-- Seed for the docker-compose Postgres service.
-- User `birch` is the superuser created by the image, and this script runs as that user.

CREATE TABLE authors (
    id integer PRIMARY KEY,
    name text NOT NULL,
    born integer
);

CREATE TABLE books (
    id integer PRIMARY KEY,
    title text NOT NULL,
    author_id integer NOT NULL REFERENCES authors (id),
    published date,
    pages integer,
    rating double precision,
    in_print boolean NOT NULL,
    notes text
);

CREATE TABLE bookmarks (
    id integer PRIMARY KEY,
    book_id integer NOT NULL REFERENCES books (id),
    page integer NOT NULL,
    note text
);

INSERT INTO authors (id, name, born) VALUES
    (1, 'Ursula K. Le Guin', 1929),
    (2, 'Octavia E. Butler', 1947),
    (3, 'Italo Calvino', 1923),
    (4, 'Jorge Luis Borges', 1899);

INSERT INTO books (id, title, author_id, published, pages, rating, in_print, notes) VALUES
    (1, 'The Left Hand of Darkness', 1, DATE '1969-03-01', 304, 4.6, true, NULL),
    (2, 'The Dispossessed', 1, DATE '1974-05-01', 387, 4.5, true, 'First of the Hainish novels many people meet second.'),
    (3, 'Kindred', 2, DATE '1979-06-01', 264, 4.7, true, NULL),
    (4, 'Parable of the Sower', 2, DATE '1993-10-01', 345, 4.4, true, 'Earthseed.'),
    (5, 'Invisible Cities', 3, DATE '1972-01-01', 165, 4.3, true, NULL),
    (6, 'If on a winter''s night a traveler', 3, DATE '1979-06-01', 260, 4.1, false, 'Out of the sample print run.'),
    (7, 'Ficciones', 4, DATE '1944-01-01', 174, 4.8, true, NULL),
    (8, 'The Book of Sand', 4, DATE '1975-01-01', 156, 4.2, true, NULL);

INSERT INTO bookmarks (id, book_id, page, note) VALUES
    (1, 1, 42, 'Winter'),
    (2, 3, 18, NULL),
    (3, 7, 90, 'The Library of Babel');

CREATE VIEW books_in_print AS
SELECT books.id, books.title, authors.name AS author, books.pages, books.rating
FROM books
JOIN authors ON authors.id = books.author_id
WHERE books.in_print;

CREATE SCHEMA IF NOT EXISTS archive;

CREATE TABLE archive.retired_books (
    id integer PRIMARY KEY,
    title text NOT NULL,
    retired_on date NOT NULL
);

INSERT INTO archive.retired_books (id, title, retired_on) VALUES
    (1, 'The Magazine of Fantasy and Science Fiction, April 1969', DATE '1990-01-01');

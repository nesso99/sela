-- Seed for the docker-compose MySQL service.
-- The entrypoint runs this as root, so it can grant the sample user a second database.

CREATE TABLE authors (
    id INT PRIMARY KEY,
    name VARCHAR(255) NOT NULL,
    born INT
);

CREATE TABLE books (
    id INT PRIMARY KEY,
    title VARCHAR(255) NOT NULL,
    author_id INT NOT NULL,
    published DATE,
    pages INT,
    rating DOUBLE,
    in_print TINYINT(1) NOT NULL,
    notes TEXT,
    CONSTRAINT books_author_fk FOREIGN KEY (author_id) REFERENCES authors (id)
);

CREATE TABLE bookmarks (
    id INT PRIMARY KEY,
    book_id INT NOT NULL,
    page INT NOT NULL,
    note VARCHAR(255),
    CONSTRAINT bookmarks_book_fk FOREIGN KEY (book_id) REFERENCES books (id)
);

INSERT INTO authors (id, name, born) VALUES
    (1, 'Ursula K. Le Guin', 1929),
    (2, 'Octavia E. Butler', 1947),
    (3, 'Italo Calvino', 1923),
    (4, 'Jorge Luis Borges', 1899);

INSERT INTO books (id, title, author_id, published, pages, rating, in_print, notes) VALUES
    (1, 'The Left Hand of Darkness', 1, '1969-03-01', 304, 4.6, 1, NULL),
    (2, 'The Dispossessed', 1, '1974-05-01', 387, 4.5, 1, 'First of the Hainish novels many people meet second.'),
    (3, 'Kindred', 2, '1979-06-01', 264, 4.7, 1, NULL),
    (4, 'Parable of the Sower', 2, '1993-10-01', 345, 4.4, 1, 'Earthseed.'),
    (5, 'Invisible Cities', 3, '1972-01-01', 165, 4.3, 1, NULL),
    (6, 'If on a winter''s night a traveler', 3, '1979-06-01', 260, 4.1, 0, 'Out of the sample print run.'),
    (7, 'Ficciones', 4, '1944-01-01', 174, 4.8, 1, NULL),
    (8, 'The Book of Sand', 4, '1975-01-01', 156, 4.2, 1, NULL);

INSERT INTO bookmarks (id, book_id, page, note) VALUES
    (1, 1, 42, 'Winter'),
    (2, 3, 18, NULL),
    (3, 7, 90, 'The Library of Babel');

CREATE VIEW books_in_print AS
SELECT books.id, books.title, authors.name AS author, books.pages, books.rating
FROM books
JOIN authors ON authors.id = books.author_id
WHERE books.in_print = 1;

CREATE DATABASE IF NOT EXISTS archive;
GRANT ALL PRIVILEGES ON archive.* TO 'sela'@'%';

CREATE TABLE archive.retired_books (
    id INT PRIMARY KEY,
    title VARCHAR(255) NOT NULL,
    retired_on DATE NOT NULL
);

INSERT INTO archive.retired_books (id, title, retired_on) VALUES
    (1, 'The Magazine of Fantasy and Science Fiction, April 1969', '1990-01-01');

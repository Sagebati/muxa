CREATE TABLE widgets (
    id       SERIAL PRIMARY KEY,
    name     TEXT NOT NULL,
    quantity INTEGER NOT NULL DEFAULT 0
);

INSERT INTO widgets (name, quantity) VALUES
    ('sprocket', 5),
    ('gizmo', 12);

//! Integration coverage for collaborative redo, author bounds, eraser strokes,
//! laser pointers, shortcut buttons, and premultiplied reference output.

use fm_telestrator::{
    ApplyOutcome, AuthorId, Board, BoardId, Color, LaserOutcome, LaserPointer, Limits, ModelError,
    Operation, OperationId, OperationKind, Point, PointId, SHORTCUT_SLOTS, ShortcutBank, Stroke,
    StrokeId, StrokeKind, StrokeStyle, ToolPreset, UndoScope,
};

const BOARD: BoardId = BoardId(7);
const ALICE: AuthorId = AuthorId(1);
const BOB: AuthorId = AuthorId(2);
const CAROL: AuthorId = AuthorId(3);
const RED: Color = Color::rgba(255, 0, 0, 255);
const GREEN: Color = Color::rgba(0, 255, 0, 255);
const BLUE: Color = Color::rgba(0, 0, 255, 255);

struct Session {
    board: Board,
    next: u64,
}

impl Session {
    fn new() -> Self {
        Self::with(Board::new(BOARD))
    }

    fn with(board: Board) -> Self {
        Self { board, next: 1 }
    }

    fn op(&self, author: AuthorId, kind: OperationKind) -> Operation {
        Operation::new(
            OperationId(u128::from(self.next) + 1_000),
            BOARD,
            self.next,
            author,
            kind,
        )
    }

    fn try_apply(
        &mut self,
        author: AuthorId,
        kind: OperationKind,
    ) -> Result<ApplyOutcome, ModelError> {
        let result = self.board.apply(self.op(author, kind));
        if result.is_ok() {
            self.next += 1;
        }
        result
    }

    fn apply(&mut self, author: AuthorId, kind: OperationKind) -> ApplyOutcome {
        self.try_apply(author, kind).expect("operation applies")
    }

    fn ids(&self) -> Vec<u128> {
        self.board
            .strokes()
            .iter()
            .map(|stroke| stroke.id().get())
            .collect()
    }
}

fn point(id: u128, x: f32, y: f32) -> Point {
    Point::new(PointId(id), x, y).unwrap()
}

fn line(
    id: u128,
    author: AuthorId,
    color: Color,
    opacity: f32,
    from: (f32, f32),
    to: (f32, f32),
) -> Stroke {
    Stroke::new(
        StrokeId(id),
        author,
        StrokeKind::Line,
        StrokeStyle::new(color, 0.2, opacity).unwrap(),
        vec![
            point(id * 10, from.0, from.1),
            point(id * 10 + 1, to.0, to.1),
        ],
    )
    .unwrap()
}

fn freehand(id: u128, author: AuthorId, kind: StrokeKind, opacity: f32, at: (f32, f32)) -> Stroke {
    Stroke::new(
        StrokeId(id),
        author,
        kind,
        StrokeStyle::new(RED, 0.2, opacity).unwrap(),
        vec![point(id * 10, at.0, at.1)],
    )
    .unwrap()
}

fn horizontal_red(id: u128, author: AuthorId) -> Stroke {
    line(id, author, RED, 1.0, (0.0, 0.5), (1.0, 0.5))
}

#[test]
fn author_scoped_undo_and_redo_restore_points_and_order() {
    let mut s = Session::new();
    s.apply(
        ALICE,
        OperationKind::Add(freehand(1, ALICE, StrokeKind::Freehand, 1.0, (0.1, 0.1))),
    );
    s.apply(BOB, OperationKind::Add(horizontal_red(2, BOB)));
    s.apply(
        ALICE,
        OperationKind::Update {
            stroke_id: StrokeId(1),
            points: vec![point(11, 0.2, 0.2), point(12, 0.3, 0.3)],
        },
    );

    assert_eq!(
        s.apply(ALICE, OperationKind::Undo(UndoScope::Author)),
        ApplyOutcome::Applied
    );
    assert_eq!(s.board.stroke(StrokeId(1)).unwrap().points().len(), 1);
    assert_eq!(s.board.redo_history_len(), 1);

    assert_eq!(
        s.apply(ALICE, OperationKind::Redo(UndoScope::Author)),
        ApplyOutcome::Applied
    );
    let ids: Vec<_> = s
        .board
        .stroke(StrokeId(1))
        .unwrap()
        .points()
        .iter()
        .map(|point| point.id.get())
        .collect();
    assert_eq!(ids, [10, 11, 12]);

    // Undo update then add: the stroke disappears; Bob's stroke is untouched.
    s.apply(ALICE, OperationKind::Undo(UndoScope::Author));
    s.apply(ALICE, OperationKind::Undo(UndoScope::Author));
    assert_eq!(s.ids(), [2]);
    assert_eq!(
        s.apply(ALICE, OperationKind::Undo(UndoScope::Author)),
        ApplyOutcome::NothingToUndo
    );

    // Bob has nothing to redo; Alice's redo reinserts at the original index.
    assert_eq!(
        s.apply(BOB, OperationKind::Redo(UndoScope::Author)),
        ApplyOutcome::NothingToRedo
    );
    s.apply(ALICE, OperationKind::Redo(UndoScope::Author));
    assert_eq!(s.ids(), [1, 2]);
    assert_eq!(s.board.stroke(StrokeId(1)).unwrap().points().len(), 1);
    s.apply(ALICE, OperationKind::Redo(UndoScope::Author));
    assert_eq!(s.board.stroke(StrokeId(1)).unwrap().points().len(), 3);
    assert_eq!(
        s.apply(ALICE, OperationKind::Redo(UndoScope::Author)),
        ApplyOutcome::NothingToRedo
    );
    // Redo/no-op redo still consume sequence numbers in the ordered log.
    assert_eq!(s.board.last_sequence(), s.next - 1);
}

#[test]
fn new_mutation_discards_only_that_authors_redo() {
    let mut s = Session::new();
    s.apply(ALICE, OperationKind::Add(horizontal_red(1, ALICE)));
    s.apply(
        BOB,
        OperationKind::Add(line(2, BOB, BLUE, 1.0, (0.5, 0.0), (0.5, 1.0))),
    );
    s.apply(ALICE, OperationKind::Undo(UndoScope::Author));
    s.apply(BOB, OperationKind::Undo(UndoScope::Author));
    assert_eq!(s.board.redo_history_len(), 2);

    s.apply(ALICE, OperationKind::Add(horizontal_red(3, ALICE)));
    assert_eq!(s.board.redo_history_len(), 1);
    assert_eq!(
        s.apply(ALICE, OperationKind::Redo(UndoScope::Author)),
        ApplyOutcome::NothingToRedo
    );
    assert_eq!(
        s.apply(BOB, OperationKind::Redo(UndoScope::Author)),
        ApplyOutcome::Applied
    );
    // Bob's stroke was at index 0 when undone, so it is reinserted there.
    assert_eq!(s.ids(), [2, 3]);
}

#[test]
fn global_undo_and_redo_follow_board_order() {
    let mut s = Session::new();
    s.apply(ALICE, OperationKind::Add(horizontal_red(1, ALICE)));
    s.apply(BOB, OperationKind::Add(horizontal_red(2, BOB)));
    s.apply(CAROL, OperationKind::Undo(UndoScope::Global));
    s.apply(CAROL, OperationKind::Undo(UndoScope::Global));
    assert_eq!(s.board.strokes(), []);
    // Global redo replays the most recently undone mutation first.
    s.apply(CAROL, OperationKind::Redo(UndoScope::Global));
    assert_eq!(s.ids(), [1]);
    s.apply(CAROL, OperationKind::Redo(UndoScope::Global));
    assert_eq!(s.ids(), [1, 2]);
    // Redone mutations keep their original authors for author-scoped undo.
    s.apply(BOB, OperationKind::Undo(UndoScope::Author));
    assert_eq!(s.ids(), [1]);
}

#[test]
fn redo_of_clear_preserves_strokes_added_after_the_undo() {
    let mut s = Session::new();
    s.apply(ALICE, OperationKind::Add(horizontal_red(1, ALICE)));
    s.apply(ALICE, OperationKind::Add(horizontal_red(2, ALICE)));
    s.apply(BOB, OperationKind::Clear);
    s.apply(BOB, OperationKind::Undo(UndoScope::Author));
    assert_eq!(s.ids(), [1, 2]);
    s.apply(CAROL, OperationKind::Add(horizontal_red(3, CAROL)));

    s.apply(BOB, OperationKind::Redo(UndoScope::Author));
    assert_eq!(s.ids(), [3]);
    s.apply(BOB, OperationKind::Undo(UndoScope::Author));
    assert_eq!(s.ids(), [1, 2, 3]);
}

#[test]
fn conflicting_redo_is_rejected_without_consuming_sequence() {
    let mut s = Session::new();
    s.apply(ALICE, OperationKind::Add(horizontal_red(1, ALICE)));
    s.apply(
        BOB,
        OperationKind::Delete {
            stroke_id: StrokeId(1),
        },
    );
    s.apply(BOB, OperationKind::Undo(UndoScope::Author));
    assert_eq!(s.ids(), [1]);
    s.apply(
        ALICE,
        OperationKind::Delete {
            stroke_id: StrokeId(1),
        },
    );

    let before = s.board.last_sequence();
    assert_eq!(
        s.try_apply(BOB, OperationKind::Redo(UndoScope::Author)),
        Err(ModelError::RedoConflict(StrokeId(1)))
    );
    assert_eq!(s.board.last_sequence(), before);
    assert_eq!(s.board.redo_history_len(), 1);
}

#[test]
fn redo_respects_stroke_limit_and_bounded_redo_history() {
    let limits = Limits::new(8, 1, 2).unwrap();
    let mut s = Session::with(Board::with_limits(BOARD, limits).unwrap());
    s.apply(ALICE, OperationKind::Add(horizontal_red(1, ALICE)));
    s.apply(ALICE, OperationKind::Undo(UndoScope::Author));
    s.apply(BOB, OperationKind::Add(horizontal_red(2, BOB)));
    assert_eq!(
        s.try_apply(ALICE, OperationKind::Redo(UndoScope::Author)),
        Err(ModelError::StrokeLimitExceeded)
    );

    let mut s = Session::with(Board::with_limits(BOARD, Limits::new(8, 8, 2).unwrap()).unwrap());
    for id in 1..=3 {
        s.apply(ALICE, OperationKind::Add(horizontal_red(id, ALICE)));
    }
    for _ in 0..2 {
        s.apply(ALICE, OperationKind::Undo(UndoScope::Author));
    }
    assert_eq!(s.board.redo_history_len(), 2);
    assert!(s.board.undo_history_len() <= 2);
}

#[test]
fn author_limit_bounds_operations_and_lasers() {
    assert_eq!(
        Limits::DEFAULT.with_max_authors(0),
        Err(ModelError::InvalidLimits)
    );
    assert_eq!(
        Limits::DEFAULT.with_laser_ttl_ms(0),
        Err(ModelError::InvalidLimits)
    );
    let limits = Limits::DEFAULT.with_max_authors(2).unwrap();
    let mut s = Session::with(Board::with_limits(BOARD, limits).unwrap());
    s.apply(ALICE, OperationKind::Clear);
    let laser = LaserPointer::new(0.5, 0.5, GREEN, 0.2, 0).unwrap();
    assert_eq!(s.board.set_laser(BOB, laser), Ok(LaserOutcome::Updated));
    assert_eq!(s.board.authors(), [ALICE, BOB]);

    assert_eq!(
        s.try_apply(CAROL, OperationKind::Clear),
        Err(ModelError::AuthorLimitExceeded)
    );
    assert_eq!(
        s.board.set_laser(CAROL, laser),
        Err(ModelError::AuthorLimitExceeded)
    );
    assert_eq!(s.board.last_sequence(), 1);
    // Failed operations do not register the author.
    assert_eq!(s.board.authors(), [ALICE, BOB]);
    s.apply(BOB, OperationKind::Clear);
}

#[test]
fn laser_pointers_expire_ignore_stale_samples_and_render_on_top() {
    let limits = Limits::DEFAULT.with_laser_ttl_ms(100).unwrap();
    let mut s = Session::with(Board::with_limits(BOARD, limits).unwrap());
    s.apply(ALICE, OperationKind::Add(horizontal_red(1, ALICE)));
    assert_eq!(
        LaserPointer::new(1.5, 0.5, GREEN, 0.2, 0),
        Err(ModelError::InvalidPoint)
    );
    assert_eq!(
        LaserPointer::new(0.5, 0.5, GREEN, 0.0, 0),
        Err(ModelError::InvalidWidth)
    );

    let laser = LaserPointer::new(0.5, 0.5, GREEN, 0.2, 1_000).unwrap();
    assert_eq!(s.board.set_laser(BOB, laser), Ok(LaserOutcome::Updated));
    let stale = LaserPointer::new(0.0, 0.0, BLUE, 0.2, 999).unwrap();
    assert_eq!(s.board.set_laser(BOB, stale), Ok(LaserOutcome::Stale));
    assert_eq!(s.board.active_lasers(1_099), [(BOB, laser)]);
    assert_eq!(s.board.active_lasers(1_100), []);
    // Laser samples are not sequenced or undoable.
    assert_eq!(s.board.last_sequence(), 1);
    assert_eq!(s.board.undo_history_len(), 1);

    let lit = s.board.render_with_lasers(5, 5, 1_050).unwrap();
    assert_eq!(lit.pixel(2, 2), Some([0, 255, 0, 255]));
    assert_eq!(lit.pixel(1, 2), Some([255, 0, 0, 255]));
    assert_eq!(lit.pixel(2, 1), Some([0, 0, 0, 0]));
    let expired = s.board.render_with_lasers(5, 5, 1_100).unwrap();
    assert_eq!(expired, s.board.render(5, 5).unwrap());
    assert_eq!(expired.pixel(2, 2), Some([255, 0, 0, 255]));

    assert_eq!(s.board.prune_lasers(1_099), 0);
    assert_eq!(s.board.prune_lasers(1_100), 1);
    assert!(!s.board.clear_laser(BOB));
    s.board.set_laser(BOB, laser).unwrap();
    assert!(s.board.clear_laser(BOB));
    assert_eq!(s.board.active_lasers(1_000), []);
}

#[test]
fn eraser_removes_prior_coverage_exactly_and_is_undoable() {
    let mut s = Session::new();
    s.apply(ALICE, OperationKind::Add(horizontal_red(1, ALICE)));
    s.apply(
        BOB,
        OperationKind::Add(freehand(2, BOB, StrokeKind::Eraser, 1.0, (0.5, 0.5))),
    );
    // A one-point eraser may be finalized like freehand ink.
    s.apply(
        BOB,
        OperationKind::Finalize {
            stroke_id: StrokeId(2),
        },
    );
    assert!(!StrokeKind::Eraser.requires_two_points());
    assert!(StrokeKind::Arrow.requires_two_points());

    let layer = s.board.render(5, 5).unwrap();
    assert_eq!(layer.pixel(2, 2), Some([0, 0, 0, 0]));
    assert_eq!(layer.pixel(1, 2), Some([255, 0, 0, 255]));
    assert_eq!(layer.pixel(3, 2), Some([255, 0, 0, 255]));

    // Strokes drawn after the eraser are not erased.
    s.apply(
        ALICE,
        OperationKind::Add(line(3, ALICE, BLUE, 1.0, (0.5, 0.0), (0.5, 1.0))),
    );
    assert_eq!(
        s.board.render(5, 5).unwrap().pixel(2, 2),
        Some([0, 0, 255, 255])
    );
    s.apply(ALICE, OperationKind::Undo(UndoScope::Author));

    // Undoing the finalize and the add brings the ink back.
    s.apply(BOB, OperationKind::Undo(UndoScope::Author));
    s.apply(BOB, OperationKind::Undo(UndoScope::Author));
    assert_eq!(
        s.board.render(5, 5).unwrap().pixel(2, 2),
        Some([255, 0, 0, 255])
    );
}

#[test]
fn partial_eraser_reduces_alpha_without_changing_color() {
    let mut s = Session::new();
    s.apply(ALICE, OperationKind::Add(horizontal_red(1, ALICE)));
    s.apply(
        ALICE,
        OperationKind::Add(freehand(2, ALICE, StrokeKind::Eraser, 0.5, (0.5, 0.5))),
    );
    let layer = s.board.render(5, 5).unwrap();
    // Erase alpha = round(255 * 0.5) = 128; 255 * (255 - 128) / 255 = 127.
    assert_eq!(layer.pixel(2, 2), Some([255, 0, 0, 127]));
    assert_eq!(
        &layer.to_premultiplied_rgba()[(2 * 5 + 2) * 4..(2 * 5 + 3) * 4],
        &[127, 0, 0, 127]
    );
    // Erasing an empty pixel stays transparent black.
    let mut empty = Session::new();
    empty.apply(
        ALICE,
        OperationKind::Add(freehand(1, ALICE, StrokeKind::Eraser, 1.0, (0.5, 0.5))),
    );
    assert!(
        empty
            .board
            .render(5, 5)
            .unwrap()
            .pixels()
            .iter()
            .all(|&byte| byte == 0)
    );
}

#[test]
fn premultiplied_export_matches_reference_pixels() {
    let mut s = Session::new();
    s.apply(
        ALICE,
        OperationKind::Add(line(1, ALICE, RED, 0.5, (0.0, 0.5), (1.0, 0.5))),
    );
    s.apply(
        BOB,
        OperationKind::Add(line(2, BOB, BLUE, 0.5, (0.5, 0.0), (0.5, 1.0))),
    );
    let layer = s.board.render(5, 5).unwrap();
    let premultiplied = layer.to_premultiplied_rgba();
    assert_eq!(premultiplied.len(), layer.pixels().len());
    let at = |x: usize, y: usize| &premultiplied[(y * 5 + x) * 4..(y * 5 + x + 1) * 4];

    assert_eq!(layer.pixel(2, 2), Some([85, 0, 170, 192]));
    assert_eq!(at(2, 2), [64, 0, 128, 192]);
    assert_eq!(layer.pixel(0, 2), Some([255, 0, 0, 128]));
    assert_eq!(at(0, 2), [128, 0, 0, 128]);
    assert_eq!(layer.pixel(2, 4), Some([0, 0, 255, 128]));
    assert_eq!(at(2, 4), [0, 0, 128, 128]);
    assert_eq!(at(0, 0), [0, 0, 0, 0]);
    for pixel in premultiplied.as_chunks::<4>().0 {
        assert!(pixel[..3].iter().all(|&channel| channel <= pixel[3]));
    }
}

#[test]
fn replaying_the_same_log_renders_identical_bytes() {
    let mut first = Session::new();
    let mut log = Vec::new();
    let script = [
        (ALICE, OperationKind::Add(horizontal_red(1, ALICE))),
        (
            BOB,
            OperationKind::Add(line(2, BOB, BLUE, 0.7, (0.0, 0.0), (1.0, 1.0))),
        ),
        (
            CAROL,
            OperationKind::Add(freehand(3, CAROL, StrokeKind::Eraser, 0.6, (0.25, 0.5))),
        ),
        (ALICE, OperationKind::Undo(UndoScope::Global)),
        (ALICE, OperationKind::Redo(UndoScope::Global)),
        (BOB, OperationKind::Clear),
        (BOB, OperationKind::Undo(UndoScope::Author)),
    ];
    for (author, kind) in script {
        log.push(first.op(author, kind.clone()));
        first.apply(author, kind);
    }
    let mut second = Board::new(BOARD);
    for operation in &log {
        assert_eq!(second.apply(operation.clone()), Ok(ApplyOutcome::Applied));
    }
    // Exact replays of retained operations are idempotent.
    assert_eq!(second.apply(log[4].clone()), Ok(ApplyOutcome::Duplicate));
    assert_eq!(
        first.board.render(17, 11).unwrap(),
        second.render(17, 11).unwrap()
    );
    assert_eq!(first.ids(), [1, 2, 3]);
}

#[test]
fn shortcut_bank_has_ten_bounded_slots() {
    let bank = ShortcutBank::production_default();
    assert_eq!(SHORTCUT_SLOTS, 10);
    let kinds: Vec<_> = (0..SHORTCUT_SLOTS)
        .map(|slot| bank.get(slot).unwrap().unwrap().kind)
        .collect();
    assert_eq!(
        kinds,
        [
            StrokeKind::Freehand,
            StrokeKind::Freehand,
            StrokeKind::Freehand,
            StrokeKind::Freehand,
            StrokeKind::Arrow,
            StrokeKind::Arrow,
            StrokeKind::Ellipse,
            StrokeKind::Rectangle,
            StrokeKind::Line,
            StrokeKind::Eraser,
        ]
    );
    assert_eq!(bank.get(10), Err(ModelError::InvalidShortcutSlot(10)));

    let stroke = bank
        .stroke(
            4,
            StrokeId(9),
            ALICE,
            vec![point(1, 0.0, 0.0), point(2, 1.0, 1.0)],
        )
        .unwrap();
    assert_eq!(stroke.kind(), StrokeKind::Arrow);
    assert_eq!(stroke.style().color, Color::rgba(255, 32, 32, 255));
    assert_eq!(stroke.author(), ALICE);

    let mut custom = ShortcutBank::default();
    assert_eq!(
        custom.stroke(0, StrokeId(1), ALICE, vec![point(1, 0.5, 0.5)]),
        Err(ModelError::ShortcutUnassigned(0))
    );
    let preset =
        ToolPreset::new(StrokeKind::Line, StrokeStyle::new(GREEN, 0.2, 1.0).unwrap()).unwrap();
    assert_eq!(
        custom.assign(SHORTCUT_SLOTS, Some(preset)),
        Err(ModelError::InvalidShortcutSlot(SHORTCUT_SLOTS))
    );
    let bad = ToolPreset {
        kind: StrokeKind::Line,
        style: StrokeStyle {
            color: GREEN,
            width: 2.0,
            opacity: 1.0,
        },
    };
    assert_eq!(custom.assign(3, Some(bad)), Err(ModelError::InvalidWidth));
    custom.assign(3, Some(preset)).unwrap();

    let mut s = Session::new();
    let drawn = custom
        .stroke(
            3,
            StrokeId(1),
            ALICE,
            vec![point(1, 0.0, 0.5), point(2, 1.0, 0.5)],
        )
        .unwrap();
    s.apply(ALICE, OperationKind::Add(drawn));
    let layer = s.board.render(5, 5).unwrap();
    assert_eq!(layer.pixel(4, 2), Some([0, 255, 0, 255]));
    assert_eq!(layer.pixel(4, 1), Some([0, 0, 0, 0]));

    custom.assign(3, None).unwrap();
    assert_eq!(custom.get(3), Ok(None));
}

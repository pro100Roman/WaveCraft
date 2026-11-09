//! Finding the sync words that transmitters use in raw messages.
//!
//! A raw message starts with a preamble of alternating bits (`aa aa aa ...`)
//! and then the sync word, which is where the alternation breaks. The same bits
//! can be cut into bytes in eight ways, and the raw decoder, which has no sync
//! word to align to, may start a message a few bits early: `91 d3` then shows
//! up as `a4 74` or `47 4f` with the very same bits. A message therefore cannot
//! say alone which of these readings is the right one.
//!
//! The break itself does not depend on how the bits were cut into bytes, so
//! messages of one transmitter can be recognised by the bits at the break and
//! counted together. Most of them have the right alignment, and the reading
//! that most of them agree on is the one reported.

use std::collections::HashMap;

/// Alternating bits a message must start with for its first non-alternating
/// bit to be taken for the start of the sync word.
const MIN_PREAMBLE_BITS: usize = 16;
/// Bits that may come before the preamble: the start of a burst can be junk.
const MAX_HEAD_BITS: usize = 8;
/// Bits of a suggested sync word: two bytes.
const WORD_BITS: usize = 16;
/// Bits before the break that belong to the word of the shortest cut.
const BITS_BEFORE_BREAK: usize = 1;
/// Bits from the break on that every cut of a two-byte word covers, so that
/// the first byte after the word, which differs from frame to frame, is never
/// part of what tells transmitters apart.
const BITS_AFTER_BREAK: usize = 9;
/// Messages a word must have been seen in to be suggested; a word seen once or
/// twice is as likely a payload that happens to follow a preamble.
const MIN_MESSAGES: usize = 3;
/// Different words kept per group of messages, so that one noisy transmitter
/// cannot grow the survey without bound.
const MAX_WORDS_PER_GROUP: usize = 64;

/// A sync word that many messages have in common.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyncWordSuggestion {
    /// The word, big-endian in the low `bytes` bytes, as in `RawRadioConfig`.
    pub word: u64,
    pub bytes: u8,
    /// Messages that follow a preamble with this break, however they were cut
    /// into bytes.
    pub messages: usize,
    /// How many of those were cut into bytes the way the word is read.
    pub aligned: usize,
}

#[derive(Clone, Debug, Default)]
struct Group {
    /// Messages by the bit of their byte the break falls on.
    cuts: [usize; 8],
    /// How many messages read as each two-byte word, by that bit.
    words: HashMap<(u8, u16), usize>,
}

/// Counts the sync words in the completed raw messages of a capture.
///
/// Messages are added one at a time and are not kept, so the survey is bounded
/// however many there are.
#[derive(Clone, Debug, Default)]
pub struct SyncWordSurvey {
    groups: HashMap<u32, Group>,
}

impl SyncWordSurvey {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear(&mut self) {
        self.groups.clear();
    }

    /// Takes in the bytes of one completed message, preamble included. A
    /// message that does not start with a preamble, or ends before its sync
    /// word does, is left out.
    pub fn add(&mut self, message: &[u8]) {
        let Some(break_bit) = preamble_end(message) else {
            return;
        };
        let cut = break_bit % 8;
        let word_start = break_bit - cut;
        let total_bits = message.len() * 8;
        if word_start + WORD_BITS > total_bits || break_bit + BITS_AFTER_BREAK > total_bits {
            return;
        }
        let key = read_bits(
            message,
            break_bit - BITS_BEFORE_BREAK,
            BITS_BEFORE_BREAK + BITS_AFTER_BREAK,
        );
        let word = read_bits(message, word_start, WORD_BITS) as u16;
        let group = self.groups.entry(key).or_default();
        group.cuts[cut] += 1;
        let cut = cut as u8;
        if group.words.len() < MAX_WORDS_PER_GROUP || group.words.contains_key(&(cut, word)) {
            *group.words.entry((cut, word)).or_insert(0) += 1;
        }
    }

    /// The words seen in at least three messages, the most common first, at
    /// most `limit` of them.
    pub fn suggestions(&self, limit: usize) -> Vec<SyncWordSuggestion> {
        let mut suggestions: Vec<SyncWordSuggestion> = self
            .groups
            .values()
            .filter_map(|group| {
                let messages = group.cuts.iter().sum::<usize>();
                if messages < MIN_MESSAGES {
                    return None;
                }
                // The cut most messages share; the lowest on a tie so that the
                // result does not depend on the order of a hash map.
                let (cut, aligned) = group
                    .cuts
                    .iter()
                    .copied()
                    .enumerate()
                    .max_by_key(|&(cut, count)| (count, std::cmp::Reverse(cut)))?;
                let word = group
                    .words
                    .iter()
                    .filter(|((word_cut, _), _)| usize::from(*word_cut) == cut)
                    .max_by_key(|((_, word), count)| (**count, std::cmp::Reverse(*word)))
                    .map(|((_, word), _)| *word)?;
                Some(SyncWordSuggestion {
                    word: u64::from(word),
                    bytes: (WORD_BITS / 8) as u8,
                    messages,
                    aligned,
                })
            })
            .collect();
        suggestions.sort_by(|left, right| {
            right
                .messages
                .cmp(&left.messages)
                .then(left.word.cmp(&right.word))
        });
        // Two groups can read as the same word: a preamble of `55` breaks on a
        // different bit than one of `aa` for the same sync word. Their counts
        // add up.
        let mut merged: Vec<SyncWordSuggestion> = Vec::new();
        for suggestion in suggestions {
            match merged
                .iter_mut()
                .find(|existing| existing.word == suggestion.word)
            {
                Some(existing) => {
                    existing.messages += suggestion.messages;
                    existing.aligned += suggestion.aligned;
                }
                None => merged.push(suggestion),
            }
        }
        merged.sort_by(|left, right| {
            right
                .messages
                .cmp(&left.messages)
                .then(left.word.cmp(&right.word))
        });
        let mut suggestions = merged;
        suggestions.truncate(limit);
        suggestions
    }
}

fn bit(message: &[u8], index: usize) -> bool {
    message[index / 8] & (0x80 >> (index % 8)) != 0
}

/// `count` bits from bit `start`, most significant first.
fn read_bits(message: &[u8], start: usize, count: usize) -> u32 {
    (start..start + count).fold(0, |value, index| {
        (value << 1) | u32::from(bit(message, index))
    })
}

/// The index of the first bit that does not alternate with the one before it,
/// after a preamble of at least [`MIN_PREAMBLE_BITS`] bits that starts within
/// the first [`MAX_HEAD_BITS`].
fn preamble_end(message: &[u8]) -> Option<usize> {
    let total_bits = message.len() * 8;
    for start in 0..=MAX_HEAD_BITS {
        let mut end = start + 1;
        while end < total_bits && bit(message, end) != bit(message, end - 1) {
            end += 1;
        }
        if end - start >= MIN_PREAMBLE_BITS && end < total_bits {
            return Some(end);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn byte_bits(byte: u8) -> impl Iterator<Item = bool> {
        (0..8).map(move |index| byte & (0x80 >> index) != 0)
    }

    fn pack(bits: &[bool]) -> Vec<u8> {
        bits.chunks(8)
            .map(|chunk| {
                chunk.iter().enumerate().fold(0, |byte, (index, bit)| {
                    byte | (u8::from(*bit) << (7 - index))
                })
            })
            .collect()
    }

    /// A message the way the raw decoder reports it: `head` bits, a preamble of
    /// `preamble` bytes of `aa`, the sync word and a payload.
    fn message(head: &[bool], preamble: usize, sync: [u8; 2], payload: &[u8]) -> Vec<u8> {
        let mut bits = head.to_vec();
        for _ in 0..preamble {
            bits.extend(byte_bits(0xaa));
        }
        for byte in sync.into_iter().chain(payload.iter().copied()) {
            bits.extend(byte_bits(byte));
        }
        pack(&bits)
    }

    fn survey(messages: &[Vec<u8>]) -> SyncWordSurvey {
        let mut survey = SyncWordSurvey::new();
        for message in messages {
            survey.add(message);
        }
        survey
    }

    const PAYLOADS: [&[u8]; 4] = [
        &[0xee, 0x10, 0xc3, 0xc5, 0x7f],
        &[0xf6, 0x2b, 0xa0, 0x10, 0xd8],
        &[0xc3, 0x00, 0x11, 0x22, 0x33],
        &[0xf4, 0x7a, 0x01, 0x99, 0x42],
    ];

    #[test]
    fn a_sync_word_after_the_preamble_is_found() {
        let messages = (0..6)
            .map(|index| message(&[], 10, [0x91, 0xd3], PAYLOADS[index % 4]))
            .collect::<Vec<_>>();

        let suggestions = survey(&messages).suggestions(5);

        assert_eq!(
            suggestions,
            vec![SyncWordSuggestion {
                word: 0x91d3,
                bytes: 2,
                messages: 6,
                aligned: 6,
            }]
        );
    }

    #[test]
    fn messages_cut_into_bytes_differently_count_for_the_same_word() {
        // The decoder started two bits early in some messages, and one bit
        // early in another: `a4 74` and `c8 e9` instead of `91 d3`.
        let mut messages = (0..10)
            .map(|index| message(&[], 10, [0x91, 0xd3], PAYLOADS[index % 4]))
            .collect::<Vec<_>>();
        messages.extend(
            (0..4).map(|index| message(&[true, false], 10, [0x91, 0xd3], PAYLOADS[index % 4])),
        );
        messages.push(message(&[false], 10, [0x91, 0xd3], PAYLOADS[0]));

        let suggestions = survey(&messages).suggestions(5);

        assert_eq!(suggestions.len(), 1, "{suggestions:?}");
        assert_eq!(suggestions[0].word, 0x91d3);
        assert_eq!(suggestions[0].messages, 15);
        assert_eq!(suggestions[0].aligned, 10);
    }

    #[test]
    fn junk_before_the_preamble_does_not_hide_the_word() {
        // A bit that continues the alternation and one that does not.
        let mut messages = Vec::new();
        for (index, head) in [[true], [false], [true], [false]].iter().enumerate() {
            messages.push(message(head, 10, [0x91, 0xd3], PAYLOADS[index]));
        }

        let suggestions = survey(&messages).suggestions(5);

        assert_eq!(suggestions.len(), 1, "{suggestions:?}");
        assert_eq!(suggestions[0].messages, 4);
    }

    #[test]
    fn the_cut_most_messages_have_is_taken_for_the_right_one() {
        // Seen alone, nothing tells `a4 74` from `91 d3`: the majority does.
        let mut messages = (0..2)
            .map(|index| message(&[], 10, [0x91, 0xd3], PAYLOADS[index]))
            .collect::<Vec<_>>();
        messages
            .extend((0..3).map(|index| message(&[true, false], 10, [0x91, 0xd3], PAYLOADS[index])));

        let suggestions = survey(&messages).suggestions(5);

        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].word, 0xa474);
        assert_eq!(suggestions[0].aligned, 3);
    }

    #[test]
    fn a_word_that_starts_where_the_alternation_breaks_is_read_whole() {
        // `2d d4` continues the preamble for no bit: the break is on its
        // first bit, which a cut by two bits early would put in the byte before.
        let messages = (0..5)
            .map(|index| message(&[], 10, [0x2d, 0xd4], PAYLOADS[index % 4]))
            .collect::<Vec<_>>();

        let suggestions = survey(&messages).suggestions(5);

        assert_eq!(suggestions.len(), 1, "{suggestions:?}");
        assert_eq!(suggestions[0].word, 0x2dd4);
    }

    #[test]
    fn transmitters_are_listed_most_common_first() {
        let mut messages = (0..4)
            .map(|index| message(&[], 10, [0x2d, 0xd4], PAYLOADS[index]))
            .collect::<Vec<_>>();
        messages.extend((0..7).map(|index| message(&[], 6, [0x91, 0xd3], PAYLOADS[index % 4])));

        let suggestions = survey(&messages).suggestions(5);

        let words = suggestions
            .iter()
            .map(|suggestion| (suggestion.word, suggestion.messages))
            .collect::<Vec<_>>();
        assert_eq!(words, [(0x91d3, 7), (0x2dd4, 4)]);
        assert_eq!(survey(&messages).suggestions(1).len(), 1);
    }

    #[test]
    fn a_word_seen_in_fewer_than_three_messages_is_not_suggested() {
        let messages = (0..2)
            .map(|index| message(&[], 10, [0x91, 0xd3], PAYLOADS[index]))
            .collect::<Vec<_>>();

        assert!(survey(&messages).suggestions(5).is_empty());
    }

    #[test]
    fn what_follows_the_frame_type_is_not_part_of_the_word() {
        // The same word with a different first payload byte each time.
        let messages = [0xee, 0xf6, 0xf4, 0xf2, 0xc3, 0x00, 0xff]
            .into_iter()
            .map(|frame| message(&[true, false], 10, [0x91, 0xd3], &[frame, 0x12, 0x34]))
            .collect::<Vec<_>>();

        let suggestions = survey(&messages).suggestions(5);

        assert_eq!(suggestions.len(), 1, "{suggestions:?}");
        assert_eq!(suggestions[0].messages, 7);
    }

    #[test]
    fn messages_without_a_preamble_or_a_complete_word_are_left_out() {
        let mut survey = SyncWordSurvey::new();
        for _ in 0..5 {
            survey.add(&[]);
            survey.add(&[0xaa]);
            // No preamble: ordinary data.
            survey.add(&[0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc]);
            // A preamble that ends the message.
            survey.add(&[0xaa, 0xaa, 0xaa, 0xaa]);
            // A preamble and half a word.
            survey.add(&[0xaa, 0xaa, 0xaa, 0x91]);
        }

        assert!(survey.suggestions(5).is_empty());
    }

    #[test]
    fn a_short_preamble_is_not_a_preamble() {
        let messages = (0..5)
            .map(|index| message(&[], 1, [0x91, 0xd3], PAYLOADS[index % 4]))
            .collect::<Vec<_>>();

        assert!(survey(&messages).suggestions(5).is_empty());
    }

    #[test]
    fn noise_does_not_grow_the_survey_without_bound() {
        let mut survey = SyncWordSurvey::new();
        let mut state = 0x1234_5678_u32;
        for _ in 0..50_000 {
            let mut bytes = vec![0xaa; 4];
            for _ in 0..6 {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                bytes.push((state >> 24) as u8);
            }
            survey.add(&bytes);
        }

        assert!(survey.groups.len() <= 1 << (BITS_BEFORE_BREAK + BITS_AFTER_BREAK));
        assert!(survey
            .groups
            .values()
            .all(|group| group.words.len() <= MAX_WORDS_PER_GROUP));
    }

    #[test]
    fn the_same_word_after_another_preamble_adds_to_its_count() {
        // A preamble of 0x55 breaks on another bit of the same word.
        let mut messages = (0..3)
            .map(|index| message(&[], 10, [0x91, 0xd3], PAYLOADS[index]))
            .collect::<Vec<_>>();
        for payload in PAYLOADS {
            let mut bits = Vec::new();
            for _ in 0..10 {
                bits.extend(byte_bits(0x55));
            }
            for byte in [0x91, 0xd3].into_iter().chain(payload.iter().copied()) {
                bits.extend(byte_bits(byte));
            }
            messages.push(pack(&bits));
        }

        let suggestions = survey(&messages).suggestions(5);

        assert_eq!(suggestions.len(), 1, "{suggestions:?}");
        assert_eq!(suggestions[0].word, 0x91d3);
        assert_eq!(suggestions[0].messages, 7);
        assert_eq!(suggestions[0].aligned, 7);
    }

    #[test]
    fn one_group_keeps_a_bounded_number_of_words() {
        // The low seven bits of the word's second byte are outside what tells
        // transmitters apart, so all of these land in one group.
        let mut survey = SyncWordSurvey::new();
        for low in 0..128u8 {
            survey.add(&message(&[], 4, [0x2d, 0x80 | low], &[0x01, 0x02]));
        }

        assert_eq!(survey.groups.len(), 1);
        let group = survey.groups.values().next().unwrap();
        assert_eq!(group.words.len(), MAX_WORDS_PER_GROUP);
        assert_eq!(group.cuts.iter().sum::<usize>(), 128);
    }

    #[test]
    fn clearing_forgets_everything() {
        let messages = (0..5)
            .map(|index| message(&[], 10, [0x91, 0xd3], PAYLOADS[index % 4]))
            .collect::<Vec<_>>();
        let mut survey = survey(&messages);
        assert!(!survey.suggestions(5).is_empty());

        survey.clear();

        assert!(survey.suggestions(5).is_empty());
    }
}

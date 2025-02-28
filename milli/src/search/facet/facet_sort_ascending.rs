use heed::Result;
use roaring::RoaringBitmap;

use super::{get_first_facet_value, get_highest_level};
use crate::heed_codec::facet::{
    FacetGroupKey, FacetGroupKeyCodec, FacetGroupValue, FacetGroupValueCodec,
};
use crate::heed_codec::ByteSliceRefCodec;

/// Return an iterator which iterates over the given candidate documents in
/// ascending order of their facet value for the given field id.
///
/// The documents returned by the iterator are grouped by the facet values that
/// determined their rank. For example, given the documents:
///
/// ```ignore
/// 0: { "colour": ["blue", "green"] }
/// 1: { "colour": ["blue", "red"] }
/// 2: { "colour": ["orange", "red"] }
/// 3: { "colour": ["green", "red"] }
/// 4: { "colour": ["blue", "orange", "red"] }
/// ```
/// Then calling the function on the candidates `[0, 2, 3, 4]` will return an iterator
/// over the following elements:
/// ```ignore
/// [0, 4]  // corresponds to all the documents within the candidates that have the facet value "blue"
/// [3]     // same for "green"
/// [2]     // same for "orange"
/// END
/// ```
/// Note that once a document id is returned by the iterator, it is never returned again.
pub fn ascending_facet_sort<'t>(
    rtxn: &'t heed::RoTxn<'t>,
    db: heed::Database<FacetGroupKeyCodec<ByteSliceRefCodec>, FacetGroupValueCodec>,
    field_id: u16,
    candidates: RoaringBitmap,
) -> Result<Box<dyn Iterator<Item = Result<RoaringBitmap>> + 't>> {
    let highest_level = get_highest_level(rtxn, db, field_id)?;
    if let Some(first_bound) = get_first_facet_value::<ByteSliceRefCodec>(rtxn, db, field_id)? {
        let first_key = FacetGroupKey { field_id, level: highest_level, left_bound: first_bound };
        let iter = db.range(rtxn, &(first_key..)).unwrap().take(usize::MAX);

        Ok(Box::new(AscendingFacetSort { rtxn, db, field_id, stack: vec![(candidates, iter)] }))
    } else {
        Ok(Box::new(std::iter::empty()))
    }
}

struct AscendingFacetSort<'t, 'e> {
    rtxn: &'t heed::RoTxn<'e>,
    db: heed::Database<FacetGroupKeyCodec<ByteSliceRefCodec>, FacetGroupValueCodec>,
    field_id: u16,
    stack: Vec<(
        RoaringBitmap,
        std::iter::Take<
            heed::RoRange<'t, FacetGroupKeyCodec<ByteSliceRefCodec>, FacetGroupValueCodec>,
        >,
    )>,
}

impl<'t, 'e> Iterator for AscendingFacetSort<'t, 'e> {
    type Item = Result<RoaringBitmap>;

    fn next(&mut self) -> Option<Self::Item> {
        'outer: loop {
            let (documents_ids, deepest_iter) = self.stack.last_mut()?;
            for result in deepest_iter {
                let (
                    FacetGroupKey { level, left_bound, field_id },
                    FacetGroupValue { size: group_size, mut bitmap },
                ) = result.unwrap();
                // The range is unbounded on the right and the group size for the highest level is MAX,
                // so we need to check that we are not iterating over the next field id
                if field_id != self.field_id {
                    return None;
                }

                // If the last iterator found an empty set of documents it means
                // that we found all the documents in the sub level iterations already,
                // we can pop this level iterator.
                if documents_ids.is_empty() {
                    break;
                }

                bitmap &= &*documents_ids;
                if !bitmap.is_empty() {
                    *documents_ids -= &bitmap;

                    if level == 0 {
                        return Some(Ok(bitmap));
                    }
                    let starting_key_below =
                        FacetGroupKey { field_id: self.field_id, level: level - 1, left_bound };
                    let iter = match self.db.range(&self.rtxn, &(starting_key_below..)) {
                        Ok(iter) => iter,
                        Err(e) => return Some(Err(e.into())),
                    }
                    .take(group_size as usize);

                    self.stack.push((bitmap, iter));
                    continue 'outer;
                }
            }
            self.stack.pop();
        }
    }
}

#[cfg(test)]
mod tests {
    use roaring::RoaringBitmap;

    use crate::milli_snap;
    use crate::search::facet::facet_sort_ascending::ascending_facet_sort;
    use crate::search::facet::tests::{get_random_looking_index, get_simple_index};
    use crate::snapshot_tests::display_bitmap;

    #[test]
    fn filter_sort() {
        let indexes = [get_simple_index(), get_random_looking_index()];
        for (i, index) in indexes.iter().enumerate() {
            let txn = index.env.read_txn().unwrap();
            let candidates = (200..=300).into_iter().collect::<RoaringBitmap>();
            let mut results = String::new();
            let iter = ascending_facet_sort(&txn, index.content, 0, candidates).unwrap();
            for el in iter {
                let docids = el.unwrap();
                results.push_str(&display_bitmap(&docids));
                results.push('\n');
            }
            milli_snap!(results, i);

            txn.commit().unwrap();
        }
    }
}

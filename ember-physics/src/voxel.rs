use std::fmt;

use crate::{StaticBox, Vec3};

/// Invalid voxel-grid dimensions or storage length.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VoxelError {
    Empty,
    VolumeOverflow,
    LengthMismatch { expected: usize, actual: usize },
}

impl fmt::Display for VoxelError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("voxel dimensions must be non-zero"),
            Self::VolumeOverflow => formatter.write_str("voxel dimensions overflow usize"),
            Self::LengthMismatch { expected, actual } => {
                write!(formatter, "expected {expected} voxels, got {actual}")
            }
        }
    }
}

impl std::error::Error for VoxelError {}

/// A dense block-occupancy region used to build merged static colliders.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VoxelRegion {
    origin: [i32; 3],
    size: [usize; 3],
    solid: Vec<bool>,
}

impl VoxelRegion {
    pub fn new(origin: [i32; 3], size: [usize; 3], solid: Vec<bool>) -> Result<Self, VoxelError> {
        if size.contains(&0) {
            return Err(VoxelError::Empty);
        }
        let expected = size
            .into_iter()
            .try_fold(1usize, usize::checked_mul)
            .ok_or(VoxelError::VolumeOverflow)?;
        if solid.len() != expected {
            return Err(VoxelError::LengthMismatch {
                expected,
                actual: solid.len(),
            });
        }
        Ok(Self {
            origin,
            size,
            solid,
        })
    }

    /// Returns non-overlapping cuboids covering exactly the occupied voxels.
    #[must_use]
    pub fn merged_boxes(&self) -> Vec<StaticBox> {
        let mut consumed = vec![false; self.solid.len()];
        let mut boxes = Vec::new();

        for y in 0..self.size[1] {
            for z in 0..self.size[2] {
                for x in 0..self.size[0] {
                    if !self.available(&consumed, x, y, z) {
                        continue;
                    }

                    let x_end = self.extend_x(&consumed, x, y, z);
                    let z_end = self.extend_z(&consumed, x, x_end, y, z);
                    let y_end = self.extend_y(&consumed, x, x_end, y, z, z_end);
                    self.consume(&mut consumed, x, x_end, y, y_end, z, z_end);
                    boxes.push(self.make_box(x, x_end, y, y_end, z, z_end));
                }
            }
        }
        boxes
    }

    fn extend_x(&self, consumed: &[bool], x: usize, y: usize, z: usize) -> usize {
        let mut end = x + 1;
        while end < self.size[0] && self.available(consumed, end, y, z) {
            end += 1;
        }
        end
    }

    fn extend_z(&self, consumed: &[bool], x: usize, x_end: usize, y: usize, z: usize) -> usize {
        let mut end = z + 1;
        while end < self.size[2]
            && (x..x_end).all(|candidate_x| self.available(consumed, candidate_x, y, end))
        {
            end += 1;
        }
        end
    }

    fn extend_y(
        &self,
        consumed: &[bool],
        x: usize,
        x_end: usize,
        y: usize,
        z: usize,
        z_end: usize,
    ) -> usize {
        let mut end = y + 1;
        while end < self.size[1]
            && (z..z_end).all(|candidate_z| {
                (x..x_end)
                    .all(|candidate_x| self.available(consumed, candidate_x, end, candidate_z))
            })
        {
            end += 1;
        }
        end
    }

    #[expect(clippy::too_many_arguments)]
    fn consume(
        &self,
        consumed: &mut [bool],
        x: usize,
        x_end: usize,
        y: usize,
        y_end: usize,
        z: usize,
        z_end: usize,
    ) {
        for candidate_y in y..y_end {
            for candidate_z in z..z_end {
                for candidate_x in x..x_end {
                    consumed[self.index(candidate_x, candidate_y, candidate_z)] = true;
                }
            }
        }
    }

    fn make_box(
        &self,
        x: usize,
        x_end: usize,
        y: usize,
        y_end: usize,
        z: usize,
        z_end: usize,
    ) -> StaticBox {
        let width = (x_end - x) as f32;
        let height = (y_end - y) as f32;
        let depth = (z_end - z) as f32;
        StaticBox::new(
            Vec3::new(
                self.origin[0] as f32 + x as f32 + width * 0.5,
                self.origin[1] as f32 + y as f32 + height * 0.5,
                self.origin[2] as f32 + z as f32 + depth * 0.5,
            ),
            Vec3::new(width * 0.5, height * 0.5, depth * 0.5),
        )
    }

    fn available(&self, consumed: &[bool], x: usize, y: usize, z: usize) -> bool {
        let index = self.index(x, y, z);
        self.solid[index] && !consumed[index]
    }

    const fn index(&self, x: usize, y: usize, z: usize) -> usize {
        (y * self.size[2] + z) * self.size[0] + x
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_chunk_section_merges_into_one_box() {
        let voxels =
            VoxelRegion::new([16, -64, 32], [16, 16, 16], vec![true; 4096]).expect("valid region");
        assert_eq!(
            voxels.merged_boxes(),
            vec![StaticBox::new(
                Vec3::new(24.0, -56.0, 40.0),
                Vec3::new(8.0, 8.0, 8.0),
            )]
        );
    }

    #[test]
    fn separated_blocks_remain_separate() {
        let voxels =
            VoxelRegion::new([0, 0, 0], [3, 1, 1], vec![true, false, true]).expect("valid region");
        assert_eq!(voxels.merged_boxes().len(), 2);
    }

    #[test]
    fn rejects_wrong_storage_length() {
        let result = VoxelRegion::new([0, 0, 0], [2, 2, 2], vec![true; 7]);
        assert_eq!(
            result,
            Err(VoxelError::LengthMismatch {
                expected: 8,
                actual: 7,
            })
        );
    }
}

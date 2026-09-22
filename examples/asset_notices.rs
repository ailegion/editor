//! Export the notices embedded in the exact locked two-face version.
fn main() {
    println!("{}", two_face::acknowledgement::listing().to_md());
}

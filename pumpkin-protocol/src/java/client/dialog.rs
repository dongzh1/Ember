use pumpkin_util::text::TextComponent;
use serde::Serialize;

#[derive(Serialize)]
pub struct DialogNBT<'a>(pub DialogNBTSource<'a>);

impl<'a> DialogNBT<'a> {
    #[must_use]
    pub const fn from_dialog(dialog: &'a Dialog) -> Self {
        Self(DialogNBTSource::Struct(dialog))
    }

    #[must_use]
    pub const fn from_nbt(compound: &'a pumpkin_nbt::compound::NbtCompound) -> Self {
        Self(DialogNBTSource::Nbt(compound))
    }
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum DialogNBTSource<'a> {
    Struct(&'a Dialog),
    Nbt(&'a pumpkin_nbt::compound::NbtCompound),
}

#[derive(Serialize)]
pub struct Dialog {
    pub r#type: String,
    pub title: TextComponent,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub body: Vec<DialogBody>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<DialogInput>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub buttons: Vec<ActionButton>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<DialogLink>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_action: Option<DialogAction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after_action: Option<String>,
    pub can_close_with_escape: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_title: Option<TextComponent>,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DialogBody {
    #[serde(rename = "minecraft:plain_message")]
    PlainMessage { contents: TextComponent },
    #[serde(rename = "minecraft:item")]
    Item { item: i32 }, // TODO: ItemStack serialization to NBT
}

// EMBER start - real dialog input collection (key + minecraft:dynamic/custom)
//
// The wire shape of every variant here (including `key`) was verified
// directly against the Minecraft Wiki's Dialog page, not guessed: every
// real input control requires a `key` - "String identifier of value used
// when submitting data" - which is how a `minecraft:dynamic/custom` button
// (see `DialogAction::DynamicCustom`) labels each input's value in the NBT
// compound it sends back. Ember's `Text` variant previously had
// `placeholder`/`default_value` fields that don't exist in the real
// protocol (never had any effect) - reshaped to the real
// `key`/`label`/`initial`/`max_length` fields instead.
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DialogInput {
    #[serde(rename = "minecraft:boolean")]
    Boolean {
        key: String,
        label: TextComponent,
        default_value: bool,
    },
    #[serde(rename = "minecraft:text")]
    Text {
        key: String,
        label: TextComponent,
        initial: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        max_length: Option<u32>,
    },
    #[serde(rename = "minecraft:number_range")]
    NumberRange {
        key: String,
        label: TextComponent,
        min: f32,
        max: f32,
        initial: f32,
        step: f32,
        #[serde(skip_serializing_if = "Option::is_none")]
        label_format: Option<String>,
    },
    #[serde(rename = "minecraft:single_option")]
    SingleOption {
        key: String,
        label: TextComponent,
        options: Vec<TextComponent>,
        initial_index: u32,
    },
}
// EMBER end

#[derive(Serialize)]
pub struct ActionButton {
    pub text: TextComponent,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tooltip: Option<TextComponent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    pub action: DialogAction,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DialogAction {
    #[serde(rename = "minecraft:open_url")]
    OpenUrl { url: String },
    /// Static custom event: the client echoes back exactly `payload`
    /// unchanged, regardless of what (if anything) is in the dialog's
    /// `inputs` - per the Minecraft Wiki, has no effect on input
    /// collection. Use [`DynamicCustom`](Self::DynamicCustom) to actually
    /// read what the player typed/selected.
    #[serde(rename = "minecraft:custom")]
    Custom {
        id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        payload: Option<Vec<u8>>,
    },
    // EMBER start - real dialog input collection (key + minecraft:dynamic/custom)
    /// Dynamic custom event: the client builds an NBT compound from every
    /// input control's current value, keyed by that input's own `key`
    /// field (see `DialogInput`), merges in `additions` (extra static
    /// fields the server wants alongside the collected ones), and sends
    /// the result back - this is the only action type that actually
    /// carries player-typed/selected dialog input values to the server.
    /// Decode a submission with `decode_dialog_submission`.
    #[serde(rename = "minecraft:dynamic/custom")]
    DynamicCustom {
        id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        additions: Option<pumpkin_nbt::compound::NbtCompound>,
    },
    // EMBER end
}

// EMBER start - real dialog input collection (key + minecraft:dynamic/custom)
/// Decodes a `minecraft:dynamic/custom` submission (see
/// [`DialogAction::DynamicCustom`]) out of the raw bytes carried in
/// `SCustomClickAction`'s `payload`.
///
/// This is the client-built NBT compound containing every input's current
/// value, keyed by that input's own `key` field. Reads it as unnamed-root
/// NBT (this crate's own `from_bytes_unnamed` doc comment says it's for
/// "network NBT", and the real protocol has sent unnamed-root NBT over the
/// wire since 1.20.2) - **not empirically verified against a real client**
/// (none was available while writing this).
///
/// A named-root/unnamed-root auto-detecting fallback was tried first and
/// deliberately dropped: `from_bytes_unnamed` does not reliably *error* on
/// named-root bytes, it silently misparses the leading empty-name-length
/// prefix as if it were payload and returns a wrong-but-successful empty
/// compound (proven by this file's own round-trip test failing that way),
/// so an error-triggered fallback can silently swallow real submissions
/// instead of ever using the correct path. If real-client testing (see
/// this crate's changelog / `EMBER.md` for the suggested `/dialog show`
/// dry run) shows this guess is wrong, swap `from_bytes_unnamed` for
/// `from_bytes` here - a one-line fix, not a design change.
///
/// # Errors
/// Returns the underlying NBT decode error if `payload` isn't valid
/// unnamed-root NBT.
pub fn decode_dialog_submission(
    payload: &[u8],
) -> Result<pumpkin_nbt::compound::NbtCompound, pumpkin_nbt::Error> {
    use std::io::Cursor;
    pumpkin_nbt::from_bytes_unnamed(Cursor::new(payload))
}
// EMBER end

#[derive(Serialize)]
pub struct DialogLink {
    pub label: crate::Label,
    pub url: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    // EMBER: regression test for a real crash - `DialogNBT` (and `Dialog`'s
    // own `TextComponent` fields) are newtype structs, and Ember's NBT
    // serializer used to reject those unconditionally
    // (`pumpkin_nbt::serializer::Serializer::serialize_newtype_struct`).
    // First hit live by `server::auth`'s notice dialog - the exact shape
    // reproduced here - which crashed the whole server the moment a player
    // actually triggered it (never exercised before then).
    #[test]
    fn dialog_with_text_components_serializes_as_nbt() {
        let dialog = Dialog {
            r#type: "minecraft:notice".to_string(),
            title: TextComponent::text("请登录"),
            body: vec![DialogBody::PlainMessage {
                contents: TextComponent::text("请输入密码登录你的账户。"),
            }],
            inputs: vec![],
            buttons: vec![ActionButton {
                text: TextComponent::text("开始"),
                tooltip: None,
                width: None,
                action: DialogAction::Custom {
                    id: "ember:auth_ack".to_string(),
                    payload: None,
                },
            }],
            links: vec![],
            exit_action: None,
            after_action: None,
            can_close_with_escape: false,
            external_title: None,
        };

        let mut bytes = Vec::new();
        pumpkin_nbt::to_bytes(&DialogNBT::from_dialog(&dialog), &mut bytes)
            .expect("a Dialog full of TextComponent fields should serialize as NBT");
    }

    // EMBER: `inputs` was empty in every dialog Ember ever actually sent
    // before this - the first real non-empty-`inputs` + `DynamicCustom`
    // shape, so it gets the same crash-regression treatment as the test
    // above rather than trusting it compiles-therefore-works.
    #[test]
    fn dialog_with_text_inputs_and_dynamic_custom_serializes_as_nbt() {
        let dialog = Dialog {
            r#type: "minecraft:confirmation".to_string(),
            title: TextComponent::text("欢迎，测试"),
            body: vec![DialogBody::PlainMessage {
                contents: TextComponent::text("请设置一个密码来保护你的账户。"),
            }],
            inputs: vec![
                DialogInput::Text {
                    key: "password".to_string(),
                    label: TextComponent::text("密码"),
                    initial: String::new(),
                    max_length: Some(64),
                },
                DialogInput::Text {
                    key: "confirm_password".to_string(),
                    label: TextComponent::text("确认密码"),
                    initial: String::new(),
                    max_length: Some(64),
                },
            ],
            buttons: vec![ActionButton {
                text: TextComponent::text("完成注册"),
                tooltip: None,
                width: None,
                action: DialogAction::DynamicCustom {
                    id: "ember:auth/register_submit".to_string(),
                    additions: None,
                },
            }],
            links: vec![],
            exit_action: None,
            after_action: None,
            can_close_with_escape: false,
            external_title: None,
        };

        let mut bytes = Vec::new();
        pumpkin_nbt::to_bytes(&DialogNBT::from_dialog(&dialog), &mut bytes).expect(
            "a Dialog with real text inputs and a dynamic/custom button should serialize as NBT",
        );
    }

    // EMBER: self-consistency only - confirms `decode_dialog_submission`
    // reads back exactly what an `NbtCompound` encoded as unnamed-root NBT.
    // Does NOT prove this matches a real client's actual wire format - see
    // the function's own doc comment.
    #[test]
    fn decode_dialog_submission_round_trips_unnamed_root() {
        let mut compound = pumpkin_nbt::compound::NbtCompound::new();
        compound.put_string("password", "hunter2".to_string());
        compound.put_string("confirm_password", "hunter2".to_string());

        let mut bytes = Vec::new();
        pumpkin_nbt::to_bytes_unnamed(&compound, &mut bytes).unwrap();
        let decoded =
            decode_dialog_submission(&bytes).expect("unnamed-root submission should decode");
        assert_eq!(decoded.get_string("password"), Some("hunter2"));
        assert_eq!(decoded.get_string("confirm_password"), Some("hunter2"));
    }

    // EMBER: documents a real footgun found while writing the above test -
    // an earlier version of `decode_dialog_submission` tried unnamed-root
    // first and fell back to named-root on error, but named-root bytes fed
    // to the unnamed-root reader don't error, they silently misparse into
    // an empty-but-successful compound. This test pins that failure mode
    // so nobody reintroduces an error-triggered fallback without noticing
    // it doesn't actually work.
    #[test]
    fn named_root_bytes_do_not_decode_correctly_here() {
        let mut compound = pumpkin_nbt::compound::NbtCompound::new();
        compound.put_string("password", "hunter2".to_string());

        let mut named_bytes = Vec::new();
        pumpkin_nbt::to_bytes(&compound, &mut named_bytes).unwrap();
        let decoded = decode_dialog_submission(&named_bytes)
            .expect("misparses successfully instead of erroring - that's the footgun");
        assert_ne!(
            decoded.get_string("password"),
            Some("hunter2"),
            "if this now passes, named-root decoding started working - update \
             decode_dialog_submission's doc comment and this test together"
        );
    }
}

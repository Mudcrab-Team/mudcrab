use bevy::prelude::*;

#[derive(Component, Default, Clone)]
pub struct PlayButton;

/// The line beside the Play button that says why it is, or is not, ready.
#[derive(Component, Default, Clone)]
pub struct PlayHintText;

// ABOUTME: Defines one action table for SOAP admission and downloadable service descriptions.
// ABOUTME: Associates every wire argument with its service state variable.
use super::Service;

pub(super) struct ActionSpec {
    pub name: &'static str,
    pub input: &'static [&'static str],
    pub output: &'static [(&'static str, &'static str)],
}

macro_rules! action {
    ($name:literal, [$($input:literal),*], [$($output:literal => $variable:literal),*]) => {
        ActionSpec { name: $name, input: &[$($input),*], output: &[$(($output, $variable)),*] }
    };
}

const AVT: &[ActionSpec] = &[
    action!(
        "SetAVTransportURI",
        ["InstanceID", "CurrentURI", "CurrentURIMetaData"],
        []
    ),
    action!("GetMediaInfo", ["InstanceID"], ["NrTracks" => "NumberOfTracks", "MediaDuration" => "CurrentMediaDuration", "CurrentURI" => "AVTransportURI", "CurrentURIMetaData" => "AVTransportURIMetaData", "NextURI" => "NextAVTransportURI", "NextURIMetaData" => "NextAVTransportURIMetaData", "PlayMedium" => "PlaybackStorageMedium", "RecordMedium" => "RecordStorageMedium", "WriteStatus" => "RecordMediumWriteStatus"]),
    action!("GetTransportInfo", ["InstanceID"], ["CurrentTransportState" => "TransportState", "CurrentTransportStatus" => "TransportStatus", "CurrentSpeed" => "TransportPlaySpeed"]),
    action!("GetPositionInfo", ["InstanceID"], ["Track" => "CurrentTrack", "TrackDuration" => "CurrentTrackDuration", "TrackMetaData" => "CurrentTrackMetaData", "TrackURI" => "CurrentTrackURI", "RelTime" => "RelativeTimePosition", "AbsTime" => "AbsoluteTimePosition", "RelCount" => "RelativeCounterPosition", "AbsCount" => "AbsoluteCounterPosition"]),
    action!("GetDeviceCapabilities", ["InstanceID"], ["PlayMedia" => "PossiblePlaybackStorageMedia", "RecMedia" => "PossibleRecordStorageMedia", "RecQualityModes" => "PossibleRecordQualityModes"]),
    action!("GetTransportSettings", ["InstanceID"], ["PlayMode" => "CurrentPlayMode", "RecQualityMode" => "CurrentRecordQualityMode"]),
    action!("Stop", ["InstanceID"], []),
    action!("Play", ["InstanceID", "Speed"], []),
    action!("Pause", ["InstanceID"], []),
    action!("Seek", ["InstanceID", "Unit", "Target"], []),
    action!("Next", ["InstanceID"], []),
    action!("Previous", ["InstanceID"], []),
    action!("GetCurrentTransportActions", ["InstanceID"], ["Actions" => "CurrentTransportActions"]),
];

const RCS: &[ActionSpec] = &[
    action!("ListPresets", ["InstanceID"], ["CurrentPresetNameList" => "PresetNameList"]),
    action!("SelectPreset", ["InstanceID", "PresetName"], []),
    action!("GetMute", ["InstanceID", "Channel"], ["CurrentMute" => "Mute"]),
    action!("SetMute", ["InstanceID", "Channel", "DesiredMute"], []),
    action!("GetVolume", ["InstanceID", "Channel"], ["CurrentVolume" => "Volume"]),
    action!("SetVolume", ["InstanceID", "Channel", "DesiredVolume"], []),
    action!("GetVolumeDB", ["InstanceID", "Channel"], ["CurrentVolume" => "VolumeDB"]),
    action!(
        "SetVolumeDB",
        ["InstanceID", "Channel", "DesiredVolume"],
        []
    ),
    action!("GetVolumeDBRange", ["InstanceID", "Channel"], ["MinValue" => "VolumeDB", "MaxValue" => "VolumeDB"]),
];

const CM: &[ActionSpec] = &[
    action!("GetProtocolInfo", [], ["Source" => "SourceProtocolInfo", "Sink" => "SinkProtocolInfo"]),
    action!("GetCurrentConnectionIDs", [], ["ConnectionIDs" => "CurrentConnectionIDs"]),
    action!("GetCurrentConnectionInfo", ["ConnectionID"], ["RcsID" => "A_ARG_TYPE_RcsID", "AVTransportID" => "A_ARG_TYPE_AVTransportID", "ProtocolInfo" => "A_ARG_TYPE_ProtocolInfo", "PeerConnectionManager" => "A_ARG_TYPE_ConnectionManager", "PeerConnectionID" => "A_ARG_TYPE_ConnectionID", "Direction" => "A_ARG_TYPE_Direction", "Status" => "A_ARG_TYPE_ConnectionStatus"]),
];

impl Service {
    pub(super) const fn actions(self) -> &'static [ActionSpec] {
        match self {
            Self::AvTransport => AVT,
            Self::RenderingControl => RCS,
            Self::ConnectionManager => CM,
        }
    }
}

pub(super) fn input_variable(action: &ActionSpec, name: &str) -> &'static str {
    match name {
        "InstanceID" => "A_ARG_TYPE_InstanceID",
        "CurrentURI" => "AVTransportURI",
        "CurrentURIMetaData" => "AVTransportURIMetaData",
        "Speed" => "TransportPlaySpeed",
        "Unit" => "A_ARG_TYPE_SeekMode",
        "Target" => "A_ARG_TYPE_SeekTarget",
        "PresetName" => "A_ARG_TYPE_PresetName",
        "Channel" => "A_ARG_TYPE_Channel",
        "DesiredMute" => "Mute",
        "DesiredVolume" if action.name == "SetVolumeDB" => "VolumeDB",
        "DesiredVolume" => "Volume",
        "ConnectionID" => "A_ARG_TYPE_ConnectionID",
        _ => unreachable!("action table contains an unknown input argument"),
    }
}

pub(super) fn variable_type(name: &str) -> &'static str {
    match name {
        "A_ARG_TYPE_InstanceID" | "NumberOfTracks" | "CurrentTrack" => "ui4",
        "RelativeCounterPosition"
        | "AbsoluteCounterPosition"
        | "A_ARG_TYPE_ConnectionID"
        | "A_ARG_TYPE_RcsID"
        | "A_ARG_TYPE_AVTransportID" => "i4",
        "VolumeDB" => "i2",
        "Volume" => "ui2",
        "Mute" => "boolean",
        _ => "string",
    }
}

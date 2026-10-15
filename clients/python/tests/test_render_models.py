import pytest
from pydantic import ValidationError

from simforge_cloud.models import RenderRunInput


def test_a_2d_clip_is_not_a_complete_3d_rig() -> None:
    preview = RenderRunInput.model_validate({
        "scenarioId": "uscn_owned",
        "engine": "2d",
        "rig": {"clip": {"startSeconds": 0, "endSeconds": 2}},
    })
    request = preview.model_dump(by_alias=True, exclude_unset=True)
    request["engine"] = "3d"

    with pytest.raises(ValidationError):
        RenderRunInput.model_validate(request)
